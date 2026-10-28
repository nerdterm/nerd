# Preview fixtures

Files to look at in the file tree's preview, and that `preview::tests::the_preview_fixtures_show_what_they_say` loads.

| File | The preview shows |
|---|---|
| `picture.png` | the picture, 240×160 |
| `photo.jpg`, `picture.gif` | the same picture, converted by sips |
| `picture.svg` | the picture; `picture · source` in the header switches to its text |
| `unsized.svg` | its text: a size of `100%` cannot be read, so it is not drawn blind |
| `huge.svg` | a note: 50000×50000 is over 50 megapixels |
| `three-pages.pdf` | page 1 on white, `pdf, page 1 of 3, 612×792` |
| `poster.pdf` | a note: a 14400×14400 page is over 50 megapixels |
