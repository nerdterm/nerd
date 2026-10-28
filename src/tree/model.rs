use super::*;

impl Tree {
    /// The preview on screen now, whoever made it.
    pub fn shown_preview(&self) -> Option<&crate::preview::Preview> {
        self.preview.as_ref()
    }

    /// A tree of the workspace's own directory; `../` on top leads out of it.
    pub fn new(cwd: PathBuf) -> Self {
        let space = canon(&cwd);
        let mut t = Self {
            given: cwd,
            root: space.clone(),
            scope: space.clone(),
            space,
            on_path: HashSet::new(),
            rows: Vec::new(),
            expanded: HashSet::new(),
            shown_all: HashSet::new(),
            empty: String::new(),
            selected: 0,
            top: 0,
            mode: Mode::Tree,
            query: String::new(),
            cut: None,
            complete: None,
            typing: false,
            hidden: false,
            svg_source: false,
            focus: Focus::Tree,
            preview: None,
            pscroll: 0,
            phscroll: 0,
            wrap: false,
            preview_line: None,
            preview_on: true,
            image_view: crate::preview::ImageView::Pixels,
            marks: HashMap::new(),
            badges: Default::default(),
            changes_flat: false,
            view: View::Files,
            reloads: 0,
            shown: None,
            asked: None,
            need_asked: None,
            fresh: false,
            active: true,
            last_click: None,
        };
        t.open_root();
        t
    }

    /// Called when the space changes; the same directory is a no-op (else a `=` zoom would be
    /// undone), a different one cancels the zoom.
    pub fn set_root(&mut self, cwd: PathBuf) {
        if cwd == self.given {
            return;
        }
        self.given = cwd.clone();
        let cwd = canon(&cwd);
        if cwd == self.space {
            return;
        }
        self.space = cwd.clone();
        self.root = cwd;
        self.goto_root(self.space.clone());
    }

    /// Lists the root and puts the keys on its first entry: `../` is for leaving, not for landing.
    fn open_root(&mut self) {
        self.on_path = std::iter::successors(Some(self.root.as_path()), |d| d.parent())
            .map(Path::to_path_buf)
            .collect();
        self.top = 0;
        self.reload();
        self.selected = self.first_entry();
    }

    /// The first row that is not `../`.
    fn first_entry(&self) -> usize {
        self.rows.iter().position(|r| !r.up).unwrap_or(0)
    }

    /// Dropped in on every reload; the panel never calls git itself. In `changes` the rows
    /// are the marks, so a change that arrives has to show without a reload key. True when they moved.
    pub fn set_git_marks(&mut self, marks: HashMap<PathBuf, char>) -> bool {
        let moved = marks != self.marks;
        self.marks = marks;
        if moved && self.view == View::Changes {
            self.expand_changed();
            self.reload();
        }
        moved
    }

    /// The space's folder, which the diffs are taken in.
    pub fn space(&self) -> &Path {
        &self.space
    }

    pub fn wanted(&self) -> Option<Wanted> {
        let r = self.rows.get(self.selected)?;
        Some(Wanted { path: r.path.clone(), diff: self.view == View::Changes && !r.is_dir, reloads: self.reloads })
    }

    /// The selection is a file seen whole: not a folder, nor a search hit's line.
    pub fn whole_file(&self) -> bool {
        self.rows.get(self.selected).is_some_and(|r| !r.is_dir && r.line.is_none())
    }

    /// What to load for the preview, once per change of the selection; None while nothing new is wanted.
    pub fn next_ask(&mut self) -> Option<Wanted> {
        let w = self.wanted();
        if w == self.asked {
            return None;
        }
        self.asked = w.clone();
        w
    }

    /// A loaded preview; one for a selection already left is dropped, and the previous one stays until then.
    pub fn set_preview(&mut self, v: &mut TreeView, w: &Wanted, p: crate::preview::Preview) -> bool {
        if self.wanted().as_ref() != Some(w) {
            return false;
        }
        // A folder that will not open says so on its row as soon as the cursor has tried it, not only once expanded.
        if let Some(r) = self.rows.get_mut(self.selected)
            && r.is_dir
            && matches!(p.kind, crate::preview::Kind::Note(_))
        {
            r.err = true;
        }
        v.psel = None;
        let mut p = p;
        p.view = self.image_view;
        if let Some(l) = self.rows.get(self.selected).and_then(|r| r.line) {
            p.goto(l);
        }
        self.preview = Some(p);
        self.shown = Some(w.clone());
        // A new preview needs asking again, even for what the one before it asked.
        self.need_asked = None;
        self.fresh = true;
        true
    }

    pub fn hidden(&self) -> bool {
        self.hidden
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    #[cfg(test)]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// How images show now; the preview on screen follows at once.
    pub fn set_image_view(&mut self, view: crate::preview::ImageView) {
        self.image_view = view;
        if let Some(p) = self.preview.as_mut() {
            p.view = view;
        }
    }

    /// What the shown image needs made for the current view, once per change of that need.
    pub fn next_need(&mut self) -> Option<crate::preview::Need> {
        let n = self.preview.as_ref().and_then(|p| p.need());
        if n == self.need_asked {
            return None;
        }
        self.need_asked = n.clone();
        n
    }

    /// A PNG made for `path`; true when the preview still shows it.
    pub fn give_png(&mut self, path: &Path, png: Option<std::sync::Arc<Vec<u8>>>) -> bool {
        let Some(p) = self.preview.as_mut().filter(|p| p.path == path) else { return false };
        p.give_png(png);
        true
    }

    pub fn give_pixels(&mut self, path: &Path, px: Option<std::sync::Arc<crate::preview::Pixels>>) -> bool {
        let Some(p) = self.preview.as_mut().filter(|p| p.path == path) else { return false };
        p.give_pixels(px);
        true
    }

    /// Before a frame: scrolls to keep the selection in the last frame's room, which the draw cannot write.
    pub fn follow(&mut self, v: &TreeView) {
        if !self.preview_on {
            self.focus = Focus::Tree;
        }
        if !self.rows.is_empty() {
            self.top = view::follow_top(self.top, self.selected, self.rows.len(), v.list_h);
        }
        let row = self.rows.get(self.selected).map(|r| (r.path.clone(), r.line));
        let Some(p) = self.preview.as_mut() else { return };
        let content = view::preview_content(v.prev_area);
        // A search hit steers to its own line; two hits in the same file differ
        // only by that, so we don't reread it. Before any frame gave it room, the draw does this itself.
        if let Some((path, line)) = row
            && p.path == path
            && content.height > 0
            && (std::mem::take(&mut self.fresh) || self.preview_line != line)
        {
            self.preview_line = line;
            self.pscroll = match line {
                Some(l) => {
                    p.goto(l);
                    p.scroll_for(l, content.height)
                }
                None => 0,
            };
        }
        self.pscroll = self.pscroll.min(p.line_count().saturating_sub(1));
        self.phscroll = if self.wrap { 0 } else { self.phscroll.min(p.side_room(content.width)) };
    }

    /// Loads the wanted preview and makes what its image needs at once, as the app's workers would.
    #[cfg(test)]
    pub(crate) fn settle(&mut self, v: &mut TreeView) {
        self.set_image_view(self.image_view);
        if let Some(w) = self.wanted().filter(|w| self.shown.as_ref() != Some(w)) {
            let text = if w.diff { crate::git::diff(&self.space, &w.path) } else { String::new() };
            let p = if text.is_empty() {
                load(&w.path, self.hidden, self.svg_source)
            } else {
                crate::preview::Preview::from_text(&w.path, &text, crate::preview::Lang::Diff)
            };
            self.set_preview(v, &w, p);
        }
        while let Some(n) = self.next_need() {
            match n {
                crate::preview::Need::Png(path) => self.give_png(&path, crate::preview::to_png(&path)),
                crate::preview::Need::Pixels(path, dims) => self.give_pixels(&path, crate::preview::to_pixels(&path, dims)),
            };
        }
    }

    #[cfg(test)]
    pub fn selected_path(&self) -> Option<&Path> {
        self.rows.get(self.selected).map(|r| r.path.as_path())
    }

    /// Selects a path, expanding its ancestors within the root; used after create/rename.
    pub fn select_path(&mut self, p: &Path) {
        let mut a = p.parent();
        while let Some(d) = a {
            if d == self.root || !d.starts_with(&self.root) {
                break;
            }
            self.expanded.insert(d.to_path_buf());
            a = d.parent();
        }
        self.reload();
        // A folded chain has one row for several directories, so the row that contains the
        // path counts as the path.
        let at = self.rows.iter().position(|r| r.path == p).or_else(|| {
            self.rows.iter().position(|r| !r.up && (p.starts_with(&r.path) || r.path.starts_with(p)))
        });
        if let Some(i) = at {
            self.selected = i;
        }
    }

    pub fn svg_source(&self) -> bool {
        self.svg_source
    }

    pub fn toggle_svg_source(&mut self) {
        self.svg_source = !self.svg_source;
        self.reload();
    }

    pub fn toggle_hidden(&mut self) {
        self.hidden = !self.hidden;
        self.reload();
    }

    /// Reread from disk; there's no filesystem watching, so this runs on `r`, a root
    /// change, or after an operation.
    pub fn reload(&mut self) {
        self.rows.clear();
        // The selected file's contents may have changed too.
        self.reloads += 1;
        self.cut = None;
        match (self.view, self.mode) {
            (View::Changes, _) => self.build_changes(),
            (_, Mode::Tree) => self.build_tree(),
            (_, Mode::Filter) => self.build_filter(),
            (_, Mode::Grep) => self.build_grep(),
        }
        self.empty = if self.rows.is_empty() { empty_note(&self.root, self.hidden) } else { String::new() };
        self.clamp();
    }

    /// Refuses to delete outside the space's working directory — the tree root is the
    /// filesystem root, so guarding that would guard nothing.
    pub fn delete(&self, path: &Path) -> Result<(), String> {
        let p = canon(path);
        if p == self.space {
            return Err("cannot delete the working directory".into());
        }
        if !p.starts_with(&self.space) {
            return Err("path is outside the working directory".into());
        }
        // symlink_metadata, not metadata: a symlink to a directory must delete the
        // link, not its target.
        let md = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        if md.is_dir() {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        }
        .map_err(|e| e.to_string())
    }

    fn build_tree(&mut self) {
        let root = self.root.clone();
        if let Some(parent) = root.parent() {
            self.rows.push(Row {
                label: "../".into(),
                path: parent.to_path_buf(),
                depth: 0,
                is_dir: true,
                size: 0,
                open: false,
                link: false,
                err: false,
                line: None,
                up: true,
                more: 0,
            });
        }
        let mut chain = vec![canon(&root)];
        self.push_children(&root, 0, &mut chain);
    }

    /// The working tree of the repository, from the marks the panel is given anyway, as the
    /// tree the files sit in or as one row each.
    fn build_changes(&mut self) {
        if self.changes_flat {
            self.build_changes_flat();
        } else {
            self.build_changes_tree();
        }
    }

    /// One row per changed path, in path order, labelled relative to the root where it sits
    /// under it.
    fn build_changes_flat(&mut self) {
        let mut paths: Vec<PathBuf> = self.marks.keys().cloned().collect();
        paths.sort();
        for path in paths {
            let label = path
                .strip_prefix(&self.root)
                .map(|r| r.display().to_string())
                .unwrap_or_else(|_| crate::paths::shown(&path));
            self.rows.push(marked_row(label, path, 0));
        }
    }

    /// The same marks as a tree. Built from the paths, not from disk: a deleted file is not
    /// there to be walked into, and it is the one the tree most owes an answer about.
    fn build_changes_tree(&mut self) {
        let mut root = Node::default();
        let mut outside: Vec<PathBuf> = Vec::new();
        let mut marked: Vec<&PathBuf> = self.marks.keys().collect();
        marked.sort();
        for path in marked {
            let Ok(rel) = path.strip_prefix(&self.root) else {
                outside.push(path.clone());
                continue;
            };
            let parts: Vec<String> = rel.iter().map(|c| c.to_string_lossy().into_owned()).collect();
            let Some((name, dirs)) = parts.split_last() else { continue };
            let mut at = &mut root;
            for d in dirs {
                at = at.dirs.entry(d.clone()).or_default();
            }
            at.files.push(name.clone());
        }
        let here = self.root.clone();
        self.push_marked(&root, &here, 0);
        // A mark outside the root has no place in the tree; it still has to be named.
        for path in outside {
            self.rows.push(marked_row(crate::paths::shown(&path), path, 0));
        }
    }

    /// Emits a marked subtree: directories first, a chain of single directories folded into
    /// one row as in the file tree, then the files.
    fn push_marked(&mut self, node: &Node, at: &Path, depth: u16) {
        for (name, kid) in &node.dirs {
            let mut label = name.clone();
            let mut path = at.join(name);
            let mut kid = kid;
            while kid.files.is_empty()
                && let [(only, below)] = kid.dirs.iter().collect::<Vec<_>>().as_slice()
                && label.chars().count() + only.chars().count() < 40
            {
                label.push('/');
                label.push_str(only);
                path = path.join(only);
                kid = below;
            }
            let open = self.expanded.contains(&path);
            let mut row = marked_row(label, path.clone(), depth);
            row.is_dir = true;
            row.size = 0;
            row.open = open;
            self.rows.push(row);
            if open {
                self.push_marked(kid, &path, depth + 1);
            }
        }
        for name in &node.files {
            self.rows.push(marked_row(name.clone(), at.join(name), depth));
        }
    }

    /// Opens every directory a mark sits under: the tree of changes answers with what changed,
    /// not with the top folder of it.
    fn expand_changed(&mut self) {
        let paths: Vec<PathBuf> = self.marks.keys().cloned().collect();
        for p in paths {
            let mut a = p.parent();
            while let Some(d) = a {
                if d == self.root || !d.starts_with(&self.root) {
                    break;
                }
                self.expanded.insert(d.to_path_buf());
                a = d.parent();
            }
        }
    }

    /// Whether `changes` is one row per file rather than a tree.
    pub fn changes_flat(&self) -> bool {
        self.changes_flat
    }

    /// Flips the shape of `changes` between the tree and the flat list.
    pub fn toggle_changes_shape(&mut self) {
        self.changes_flat = !self.changes_flat;
        self.top = 0;
        self.expand_changed();
        self.reload();
        self.selected = self.first_entry();
    }

    /// Switches between the two questions the panel answers.
    pub fn set_view(&mut self, view: View) {
        if self.view == view {
            return;
        }
        self.view = view;
        self.mode = Mode::Tree;
        self.typing = false;
        self.query.clear();
        self.top = 0;
        if view == View::Changes {
            self.expand_changed();
        }
        self.reload();
        self.selected = self.first_entry();
    }

    pub fn view(&self) -> View {
        self.view
    }

    /// Searches inside collapsed directories too; the label is the path relative to
    /// the root so it's clear what matched.
    fn build_filter(&mut self) {
        let q = self.query.to_lowercase();
        if q.is_empty() {
            return self.build_tree();
        }
        // Narrowing a capped result would hide what lay past the cap, so only a complete one narrows.
        if let Some((scope, hidden, prev, rows)) = &self.complete
            && *scope == self.scope
            && *hidden == self.hidden
            && q.len() > prev.len()
            && q.starts_with(prev.as_str())
        {
            self.rows = rows.iter().filter(|r| r.label.to_lowercase().contains(&q)).cloned().collect();
            return;
        }
        let mut hits = Vec::new();
        let (end, _) = walk_names(&self.scope, self.hidden, FIND_TIME, &[], |e, rel, link| {
            if (link || e.file_type().is_ok_and(|t| t.is_file())) && rel.to_lowercase().contains(&q) {
                hits.push((e.path(), rel.to_string()));
            }
            // One past the cap tells "exactly N" from "more than N".
            hits.len() <= FIND_MAX
        });
        hits.sort_by_cached_key(|(_, rel)| tree_order(rel));
        hits.truncate(FIND_MAX);
        for (path, rel) in hits {
            // A link's own size: its target could hang on a dead mount.
            let size = std::fs::symlink_metadata(&path).map(|m| m.len()).unwrap_or(0);
            self.rows.push(found(path, rel, size, None));
        }
        self.cut = self.cut_by(end, "filter");
        self.complete = self.cut.is_none().then(|| (self.scope.clone(), self.hidden, q, self.rows.clone()));
    }

    /// Content search over the same files; binary and oversized files are skipped.
    fn build_grep(&mut self) {
        let q = self.query.to_lowercase();
        if q.is_empty() {
            return;
        }
        let mut hits = Vec::new();
        let (end, _) = walk_names(&self.scope, self.hidden, GREP_TIME, &[], |e, rel, _| {
            let path = e.path();
            let Ok(md) = std::fs::metadata(&path) else { return true };
            let size = md.len();
            if !md.is_file() || size > GREP_FILE_MAX {
                return true;
            }
            let Ok(bytes) = std::fs::read(&path) else { return true };
            if bytes[..bytes.len().min(SNIFF)].contains(&0) {
                return true;
            }
            for (i, line) in String::from_utf8_lossy(&bytes).split('\n').enumerate() {
                if !line.to_lowercase().contains(&q) {
                    continue;
                }
                let n = i as u32 + 1;
                let text: String = line.trim().chars().take(GREP_LINE).collect();
                hits.push(((tree_order(rel), n), found(path.clone(), format!("{rel}:{n}: {text}"), size, Some(n))));
                if hits.len() > GREP_MAX {
                    return false;
                }
            }
            true
        });
        hits.sort_by(|a, b| a.0.cmp(&b.0));
        hits.truncate(GREP_MAX);
        self.rows.extend(hits.into_iter().map(|(_, row)| row));
        self.cut = self.cut_by(end, "grep");
    }

    /// What stopped a walk early, if anything, logged once per walk: a silent cap looks like "nothing found".
    fn cut_by(&self, end: Walked, what: &str) -> Option<Cut> {
        let cut = match end {
            Walked::All => return None,
            Walked::Stopped => Cut::Matches(self.rows.len()),
            Walked::OutOfTime(t) => Cut::Time(t.as_millis() as u64),
        };
        crate::log::warn(&format!("tree {what}: {} in {}", cut.say(), self.scope.display()));
        Some(cut)
    }

    /// Pushes a directory's children (and expanded descendants) into `rows`. `chain`
    /// holds the branch's canonical ancestors to catch symlink cycles; returns
    /// `false` if the directory failed to read.
    fn push_children(&mut self, dir: &Path, depth: u16, chain: &mut Vec<PathBuf>) -> bool {
        let show = if self.shown_all.contains(dir) { usize::MAX } else { HUGE_ROWS };
        let Some(mut ents) = sorted_entries(dir, self.hidden, Some(&self.on_path), show) else {
            return false;
        };
        let rest = ents.len().saturating_sub(show);
        ents.truncate(show);
        for e in ents {
            // A directory whose only child is a directory draws as one row: `docs/planning/plans`.
            // ceiling: one read_dir per directory row; a listing cache if a huge tree ever drags.
            let (label, path) = self.compact(&e);
            let mut open = e.is_dir && self.expanded.contains(&path);
            // A symlink to an ancestor would loop forever; shown but not descended into.
            if open && e.link && chain.contains(&canon(&path)) {
                open = false;
            }
            self.rows.push(Row {
                label,
                path: path.clone(),
                depth,
                is_dir: e.is_dir,
                size: e.size,
                open,
                link: e.link,
                err: false,
                line: None,
                up: false,
                more: 0,
            });
            let idx = self.rows.len() - 1;
            if open {
                chain.push(canon(&path));
                let ok = self.push_children(&path, depth + 1, chain);
                chain.pop();
                if !ok {
                    self.rows[idx].err = true;
                }
            }
        }
        if rest > 0 {
            self.rows.push(Row {
                label: format!("… {rest} more, show all"),
                path: dir.to_path_buf(),
                depth,
                is_dir: false,
                size: 0,
                open: false,
                link: false,
                err: false,
                line: None,
                up: false,
                more: rest,
            });
        }
        true
    }

    /// Whether the keys are on a folder's `… more` row.
    pub(super) fn on_more(&self) -> bool {
        self.rows.get(self.selected).is_some_and(|r| r.more > 0)
    }

    /// The `… more` row at `row` gives way to the whole folder; the cursor stays where it was,
    /// on the first of the rows it hid.
    pub(super) fn show_all(&mut self, row: usize) {
        if let Some(r) = self.rows.get(row).filter(|r| r.more > 0) {
            self.shown_all.insert(r.path.clone());
            self.reload();
        }
    }

    /// Walks down a chain of single-child directories, so the whole chain is one row.
    fn compact(&self, e: &Ent) -> (String, PathBuf) {
        let mut label = e.name.clone();
        let mut path = e.path.clone();
        if !e.is_dir || e.link || is_skipped(&e.name) {
            return (label, path);
        }
        while let Some(k) = only_child(&path, self.hidden, Some(&self.on_path)) {
            if !(k.is_dir && !k.link && !is_skipped(&k.name) && label.chars().count() + k.name.chars().count() < 40) {
                break;
            }
            label.push('/');
            label.push_str(&k.name);
            path = k.path;
        }
        (label, path)
    }

    fn clamp(&mut self) {
        self.selected = if self.rows.is_empty() { 0 } else { self.selected.min(self.rows.len() - 1) };
    }

    pub(super) fn step(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let i = (self.selected as isize + delta).clamp(0, self.rows.len() as isize - 1);
        self.selected = i as usize;
    }

    /// The directory an action applies to: the selection if it's a directory, else
    /// its parent, else the root.
    pub(super) fn sel_dir(&self) -> PathBuf {
        match self.rows.get(self.selected) {
            Some(r) if r.is_dir => r.path.clone(),
            Some(r) => r.path.parent().unwrap_or(&self.root).to_path_buf(),
            None => self.root.clone(),
        }
    }

    /// Expand/collapse a directory.
    pub(super) fn toggle(&mut self, row: usize) {
        let Some(r) = self.rows.get(row) else { return };
        if !r.is_dir {
            return;
        }
        let p = r.path.clone();
        if !self.expanded.remove(&p) {
            self.expanded.insert(p);
        }
        self.reload();
    }

    /// Selects the nearest row above with a smaller depth (Left on a collapsed row).
    pub(super) fn go_parent(&mut self) {
        let Some(depth) = self.rows.get(self.selected).map(|r| r.depth) else { return };
        if depth == 0 {
            return;
        }
        if let Some(i) = (0..self.selected).rev().find(|&i| self.rows[i].depth < depth) {
            self.selected = i;
        }
    }

    /// New root: expansion and selection from the previous root don't carry over.
    pub fn goto_root(&mut self, root: PathBuf) {
        self.root = root;
        self.expanded.clear();
        self.open_root();
    }

    /// `-`: the parent becomes the root; the previous root stays expanded and selected
    /// so it's clear where we came from.
    pub(super) fn go_up(&mut self) {
        let Some(parent) = self.root.parent().map(Path::to_path_buf) else { return };
        let prev = std::mem::replace(&mut self.root, parent);
        self.expanded.clear();
        self.on_path = std::iter::successors(Some(self.root.as_path()), |d| d.parent())
            .map(Path::to_path_buf)
            .collect();
        self.expanded.insert(prev.clone());
        // Kept listed even when hidden: it is the row we land on.
        self.on_path.insert(prev.clone());
        self.top = 0;
        self.select_path(&prev);
    }

    pub(super) fn leave_query(&mut self) {
        self.mode = Mode::Tree;
        self.typing = false;
        self.query.clear();
        self.selected = 0;
        self.top = 0;
        self.reload();
    }

    /// The selected directory, or the selected file's directory.
    pub fn selected_dir(&self) -> PathBuf {
        self.sel_dir()
    }

    /// Start typing a name filter. It covers the root: typing is how you get anywhere in the
    /// panel now, and where the cursor happened to rest is no business of the search. Narrowing
    /// is what `option`+`=` is for — it moves the root.
    pub fn start_filter(&mut self, query: &str) {
        self.scope = self.root.clone();
        self.mode = Mode::Filter;
        self.typing = true;
        self.query = query.to_string();
        self.reload();
    }

    /// Content search over the root, for the same reason; the tree stays put until Enter,
    /// since grep only runs then.
    pub fn start_grep(&mut self) {
        self.scope = self.root.clone();
        self.mode = Mode::Grep;
        self.typing = true;
        self.query.clear();
        self.cut = None;
    }

    pub fn root_up(&mut self) {
        self.go_up();
    }

    /// The selected row, cloned: an action must not hold a borrow of the tree it mutates.
    pub(super) fn sel(&self) -> Option<Row> {
        self.rows.get(self.selected).filter(|r| !r.up && r.more == 0).cloned()
    }

    /// Whether the keys are on `../`.
    pub(super) fn on_up(&self) -> bool {
        self.rows.get(self.selected).is_some_and(|r| r.up)
    }
}

/// Create a file, or with a trailing `/` a directory; separators in the name are rejected.
pub fn create(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let as_dir = name.ends_with('/');
    let name = name.trim_end_matches('/');
    if !plain_name(name) {
        return Err("name is empty or has a separator".into());
    }
    let p = dir.join(name);
    if p.exists() {
        return Err(format!("{} already exists", p.display()));
    }
    let r = if as_dir { std::fs::create_dir(&p) } else { std::fs::write(&p, b"") };
    r.map(|_| p).map_err(|e| e.to_string())
}

/// Rename within the same directory.
pub fn rename(path: &Path, new_name: &str) -> Result<PathBuf, String> {
    if !plain_name(new_name) {
        return Err("name is empty or has a separator".into());
    }
    let Some(dir) = path.parent() else {
        return Err("the path has no directory".into());
    };
    let to = dir.join(new_name);
    if to.exists() {
        return Err(format!("{} already exists", to.display()));
    }
    std::fs::rename(path, &to).map(|_| to).map_err(|e| e.to_string())
}

/// Whether a string is fit to use as a name inside a directory.
fn plain_name(n: &str) -> bool {
    !n.is_empty()
        && n != "."
        && n != ".."
        && !n.contains('/')
        && !n.contains(std::path::MAIN_SEPARATOR)
}

/// A filter or search hit row: the label is ready-made, there's no tree behind it.
fn found(path: PathBuf, label: String, size: u64, line: Option<u32>) -> Row {
    Row {
        path,
        label,
        depth: 0,
        is_dir: false,
        size,
        open: false,
        link: false,
        err: false,
        line,
        up: false,
        more: 0,
    }
}

/// Directory contents sorted dirs-first, then case-insensitively by name; `None` if
/// the directory failed to read.
/// One directory entry as the tree sees it, or None for a hidden one. Hidden entries stay hidden,
/// except path components leading to the working directory (else `~/.config/app` could never expand).
fn entry(e: std::fs::DirEntry, hidden: bool, keep: Option<&HashSet<PathBuf>>) -> Option<Ent> {
    let name = e.file_name().to_string_lossy().into_owned();
    if !hidden && name.starts_with('.') && !keep.is_some_and(|k| k.contains(&e.path())) {
        return None;
    }
    let ft = e.file_type().ok()?;
    let link = ft.is_symlink();
    // file_type() doesn't follow symlinks; metadata() does, so a broken
    // link falls back to being a file.
    let md = if link { std::fs::metadata(e.path()).ok() } else { None };
    Some(Ent {
        is_dir: md.as_ref().map(|m| m.is_dir()).unwrap_or(ft.is_dir()),
        link,
        name,
        path: e.path(),
        size: md.as_ref().map(|m| m.len()).unwrap_or(0),
        changed: md.and_then(|m| m.modified().ok()),
    })
}

/// The folder's entries with the first `first` in the tree's order and statted; the rest follow
/// unsorted and unstatted, since sorting and a stat per entry is what a huge folder costs.
pub(super) fn sorted_entries(dir: &Path, hidden: bool, keep: Option<&HashSet<PathBuf>>, first: usize) -> Option<Vec<Ent>> {
    let v: Vec<Ent> = std::fs::read_dir(dir).ok()?.flatten().filter_map(|e| entry(e, hidden, keep)).collect();
    // directories first, then by name ignoring case; the key is built once per entry, not per comparison
    let mut keyed: Vec<((bool, String, String), Ent)> =
        v.into_iter().map(|e| ((!e.is_dir, e.name.to_lowercase(), e.name.clone()), e)).collect();
    if first < keyed.len() {
        keyed.select_nth_unstable_by(first, |a, b| a.0.cmp(&b.0));
        keyed[..first].sort_unstable_by(|a, b| a.0.cmp(&b.0));
    } else {
        keyed.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    }
    let mut v: Vec<Ent> = keyed.into_iter().map(|(_, e)| e).collect();
    for e in v.iter_mut().take(first).filter(|e| !e.link) {
        if let Ok(m) = std::fs::metadata(&e.path) {
            e.size = m.len();
            e.changed = m.modified().ok();
        }
    }
    Some(v)
}

/// The folder's only visible entry; None when it has none or more than one, found without reading
/// past the second, since a folder row asks this of every folder it shows.
fn only_child(dir: &Path, hidden: bool, keep: Option<&HashSet<PathBuf>>) -> Option<Ent> {
    let mut seen = std::fs::read_dir(dir).ok()?.flatten().filter_map(|e| entry(e, hidden, keep));
    let one = seen.next()?;
    seen.next().is_none().then_some(one)
}

/// A file's preview, or a folder's with its listing: what the preview worker runs; `svg_source` shows an SVG's text.
pub fn load(path: &Path, hidden: bool, svg_source: bool) -> crate::preview::Preview {
    let mut p = crate::preview::Preview::load_as(path, svg_source);
    if let crate::preview::Kind::Dir(listing) = &mut p.kind {
        *listing = Some(list(path, hidden));
    }
    p
}

/// What the preview says about a folder: its counts, and past `HUGE_ROWS` nothing that needs a stat of every entry.
fn list(dir: &Path, hidden: bool) -> crate::preview::Listing {
    let (files, folders) = count_entries(dir, hidden);
    if files + folders > HUGE_ROWS {
        return crate::preview::Listing { files, folders, bytes: None, newest: Vec::new() };
    }
    let mut ents = sorted_entries(dir, hidden, None, usize::MAX).unwrap_or_default();
    let folders = ents.iter().filter(|e| e.is_dir).count();
    let bytes = ents.iter().filter(|e| !e.is_dir).map(|e| e.size).sum();
    // Newest first: the alphabet is the tree's order, and repeating it here says nothing.
    ents.sort_by_key(|e| std::cmp::Reverse(e.changed));
    let newest = ents.iter().take(DIR_PEEK).map(|e| (e.name.clone(), e.is_dir, e.changed)).collect();
    crate::preview::Listing { files: ents.len() - folders, folders, bytes: Some(bytes), newest }
}

/// "Empty" alone leaves you guessing why; this says whether the folder failed, is hidden-only, or truly empty.
fn empty_note(root: &Path, hidden: bool) -> String {
    match std::fs::read_dir(root) {
        Err(e) => format!(" can't open this folder: {}", crate::preview::why_unreadable(&e)),
        Ok(rd) => {
            let only_hidden = !hidden && rd.flatten().any(|e| e.file_name().to_string_lossy().starts_with('.'));
            if only_hidden {
                " hidden only - ⌥+H shows them".to_string()
            } else {
                " empty - \"-\" up, \"n\" create".to_string()
            }
        }
    }
}

/// Files and folders in a folder, counted without sorting or statting them.
pub(super) fn count_entries(dir: &Path, hidden: bool) -> (usize, usize) {
    let Ok(rd) = std::fs::read_dir(dir) else { return (0, 0) };
    rd.flatten().filter_map(|e| entry(e, hidden, None)).fold((0, 0), |(f, d), e| if e.is_dir { (f, d + 1) } else { (f + 1, d) })
}

/// Why a filter or grep stopped before it saw everything.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Cut {
    Matches(usize),
    Time(u64),
}

impl Cut {
    pub(super) fn say(self) -> String {
        match self {
            Cut::Matches(n) => format!("{n} shown, more not listed"),
            Cut::Time(ms) => format!("stopped after {ms} ms"),
        }
    }
}

enum Walked {
    All,
    Stopped,
    OutOfTime(std::time::Duration),
}

/// `visit` gets (entry, path from root, is a symlink) for anything not a folder; false stops the walk. Entries are
/// classified without following a link, and the folders in `skip` are not entered; true when one was met.
fn walk_names(root: &Path, hidden: bool, budget: std::time::Duration, skip: &[PathBuf], mut visit: impl FnMut(&std::fs::DirEntry, &str, bool) -> bool) -> (Walked, bool) {
    let started = std::time::Instant::now();
    let mut skipped = false;
    let mut stack = vec![(root.to_path_buf(), String::new(), 0u16)];
    while let Some((dir, rel, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            if started.elapsed() >= budget {
                return (Walked::OutOfTime(budget), skipped);
            }
            let name = e.file_name();
            let name = name.to_string_lossy();
            if !hidden && name.starts_with('.') {
                continue;
            }
            let Ok(ft) = e.file_type() else { continue };
            let huge = is_skipped(&name);
            let r = if rel.is_empty() { name.into_owned() } else { format!("{rel}/{name}") };
            // A symlinked folder is not entered: it could be a shortcut to a cycle.
            if ft.is_dir() {
                let path = e.path();
                if skip.contains(&path) {
                    skipped = true;
                } else if depth + 1 < DEEP_DEPTH && !huge {
                    stack.push((path, r, depth + 1));
                }
            } else if !visit(&e, &r, ft.is_symlink()) {
                return (Walked::Stopped, skipped);
            }
        }
    }
    (Walked::All, skipped)
}

/// Every file under `root` from it, a link by its own name, walked as the filter walks and never into `skip`; the time it
/// ran out after, if it did, and whether a folder of `skip` was passed by.
/// ceiling: no cap but the filter's time, so a folder walked whole in it is found whole; a slower disk finds less.
pub(crate) fn file_names(root: &Path, hidden: bool, skip: &[PathBuf]) -> (Vec<String>, Option<std::time::Duration>, bool) {
    let mut out = Vec::new();
    let (end, skipped) = walk_names(root, hidden, FIND_TIME, skip, |e, rel, link| {
        if link || e.file_type().is_ok_and(|t| t.is_file()) {
            out.push(rel.to_string());
        }
        true
    });
    (out, if let Walked::OutOfTime(t) = end { Some(t) } else { None }, skipped)
}

/// The order the tree walk used to give: folders before files at each level, then by name ignoring case.
fn tree_order(rel: &str) -> Vec<(bool, String, String)> {
    let n = rel.split('/').count();
    rel.split('/').enumerate().map(|(i, p)| (i + 1 == n, p.to_lowercase(), p.to_string())).collect()
}

/// A row for a marked path. What the disk says where it is still there; a deleted file
/// leaves no metadata and is drawn as the empty file it now is.
fn marked_row(label: String, path: PathBuf, depth: u16) -> Row {
    let md = std::fs::metadata(&path).ok();
    Row {
        label,
        depth,
        is_dir: md.as_ref().is_some_and(|m| m.is_dir()),
        size: md.map(|m| m.len()).unwrap_or(0),
        open: false,
        link: false,
        err: false,
        line: None,
        up: false,
        more: 0,
        path,
    }
}

/// Git mark for a row: a file's own, or what is inside it for a directory - the one mark its
/// contents agree on, so a folder of new files reads as new, and "M" when they disagree.
/// ceiling: linear scan over all marks per directory; switch to a prefix tree if that gets slow.
pub(super) fn mark_of(marks: &HashMap<PathBuf, char>, row: &Row) -> Option<char> {
    if let Some(&m) = marks.get(&row.path) {
        return Some(m);
    }
    if !row.is_dir {
        return None;
    }
    let mut inside = marks.iter().filter(|(p, _)| p.starts_with(&row.path)).map(|(_, &m)| m);
    let first = inside.next()?;
    Some(if inside.all(|m| m == first) { first } else { 'M' })
}

/// The filesystem root - the topmost ancestor of a canonical path.
/// Canonical path, or as-is if that fails — wrong for ancestor comparisons, but
/// better than panicking.
pub(super) fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn is_skipped(name: &str) -> bool {
    SKIP.contains(&name)
}
