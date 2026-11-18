# Document ingest evaluation corpus

The answer key in `gold.json` was transcribed from the papers and written alongside
the original documents before running the preview. Its page numbers are one-based
physical PDF pages. It identifies the procedure by its OOXML heading and list
positions because DOCX pagination depends on the renderer. The JSON is test data.
Keep it separate from the extractor and query inputs.

The checkout contains the project-authored handbook and bestiary. To run the
publisher-paper examples and tests, fetch the two public PDFs from PLOS with
curl. The helper checks their SHA-256 digests and reuses an existing verified
copy. Run from the repository root.

```sh
export MINDSPACE_PREVIEW_PAPERS="${XDG_CACHE_HOME:-$HOME/.cache}/mindspace/preview-papers"
python3 -B docs/experiments/m8/document-corpus/fetch_papers.py "$MINDSPACE_PREVIEW_PAPERS"
```

| Document | Evidence to check |
| --- | --- |
| `procedure.docx` | “Reset the beacon” has three steps with automatic decimal numbering. Two station chapters each contain an Equipment section with different instructions. |
| `paper.pdf` | The two experiments have separate cohorts. On page 2 the participant counts and mean ages occur in the same sentence in the right column. The left column discusses a different experiment. |
| `stroop.pdf` | Experiment 2 on page 9 explicitly reports 24 enrolled and 20 analyzed. Nearby exclusion counts, age and subgroup size describe different quantities. Experiment 1 on page 5 reports recruitment. |
| `bestiary.pdf` | Read down each column. The Lantern Eel begins in the right column of page 1 and its resistance continues in the left column of page 2. The adjacent Glass Jackal deals cold damage but resists fire. Page 3 is a raster plate. |

The first two pages of the bestiary support two cold-resistance matches. A person
reading the plate can also see the **Silt Wren** and its conditional resistance
on page 3. A text-only query should report its two confirmed matches and the
unreadable page. This is a deliberate check against claiming complete coverage.
In `paper.pdf`, page 2 reports how many took part in Experiment 2. Its
Participants section supports the participation query. The paper does not
give explicit enrollment or analysis totals for that experiment.

## Sources and permissions

**Paper.** Tal Makovski, Gustavo A. Vázquez and Yuhong V. Jiang (2008),
*Visual Learning in Multiple-Object Tracking*, PLOS ONE 3(5), e2228.
Copyright © 2008 Makovski et al. The publisher distributes this article under
the Creative Commons Attribution License. The original article carries its
attribution and license notice on page 1. The six-page test input is the
unmodified publisher PDF. Its SHA-256 is
`b5df0cd314fec5bae87b6814574572066bf71d1887c68c1ae9794425a4e2ba89`.
[Publisher article and license](https://doi.org/10.1371/journal.pone.0002228) ·
[Publisher PDF](https://journals.plos.org/plosone/article/file?id=10.1371/journal.pone.0002228&type=printable)

**Stroop paper.** Megan C. Fitzhugh, Peter S. Whitehead, Lisa Johnson,
Julia M. Cai, Leslie C. Baxter and Corianne Rogalsky (2019), *A functional
MRI investigation of crossmodal interference in an audiovisual Stroop task*,
PLOS ONE 14(1), e0210736. Copyright © 2019 Fitzhugh et al. The tests use the
unmodified publisher PDF. The answer key records its SHA-256 and the
physical page of each count. [Publisher article](https://doi.org/10.1371/journal.pone.0210736) ·
[Publisher PDF](https://journals.plos.org/plosone/article/file?id=10.1371/journal.pone.0210736&type=printable) ·
[CC BY 4.0 license](https://creativecommons.org/licenses/by/4.0/)

**Original fixtures.** The beacon handbook, field guide, field plate, answer
key and their generator sources are original Mindspace evaluation material,
licensed under the project's [MIT License](../../../../LICENSE). All names and
creature characteristics in the field guide were invented for this test.

## Rebuilding the original fixtures

To rebuild the project-authored examples offline, run these commands in this
directory with Python 3 and Typst 0.15.1.

```sh
python3 make_procedure.py
typst compile --ignore-system-fonts --creation-timestamp 0 --ppi 100 plate.typ plate.png
typst compile --ignore-system-fonts --creation-timestamp 0 bestiary.typ bestiary.pdf
```

The plate is rasterized before embedding so the last page has no PDF text layer.
The field guide uses Typst's bundled Libertinus Serif font.
