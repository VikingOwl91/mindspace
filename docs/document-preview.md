# Document query preview

Read one trusted local PDF or DOCX and print a cited answer as JSON. Run the
preview from a Linux checkout with Python 3. PDFs also require Poppler's
`pdfinfo`, `pdftotext`, and `pdfimages`. The preview runs offline, independently
of the Mindspace database and MCP server.

## Try the handbook

Run these examples from the repository root. The original handbook is included
in the [evaluation corpus](experiments/m8/document-corpus/README.md).

```sh
python3 -B scripts/document_preview/cli.py docs/experiments/m8/document-corpus/procedure.docx section "Equipment"
python3 -B scripts/document_preview/cli.py docs/experiments/m8/document-corpus/procedure.docx section "Hill station > Equipment"
```

The handbook has an Equipment section in each of two chapters. The first query
returns both heading paths and their cited text. The second selects the hill
station's instruction to pack the wind gauge. The same syntax works for
repeated headings in a PDF.

## Try a published paper

Download the two public PDFs directly from PLOS into a local cache with curl
and the helper below. The helper verifies the expected SHA-256 for each paper.
Queries against cached files run offline. You can also pass the path to a
trusted local PDF directly.

```sh
export MINDSPACE_PREVIEW_PAPERS="${XDG_CACHE_HOME:-$HOME/.cache}/mindspace/preview-papers"
python3 -B docs/experiments/m8/document-corpus/fetch_papers.py "$MINDSPACE_PREVIEW_PAPERS"
python3 -B scripts/document_preview/cli.py "$MINDSPACE_PREVIEW_PAPERS/stroop.pdf" --profile paper cohort-counts 2
```

Experiment 2 in the Stroop paper reports 24 participants enrolled and 20
included in the analyses. Both passages are on physical page 9. Each stage in
the JSON result has its own count, status, and citations.

## Choose a query

Pass the file first, then the profile if needed, then the query and its
argument. The default profile is `general`. Text queries are case-insensitive.

| Query and example argument | Profile | Evidence returned |
| --- | --- | --- |
| `search "motion trajectories"` | Any | Extracted lines containing the literal text. |
| `section "Hill station > Equipment"` | Any | The printed heading path and section text, including subsections. A bare heading returns every matching path. |
| `procedure "Reset the beacon"` | `general` | Steps under a named heading with explicit list structure. |
| `sample-size 2` | `paper` | A participant count directly attributed to Experiment or Study 2 in a sentence or simple table. |
| `cohort-counts 2` | `paper` | Separate counts for people explicitly enrolled and people included in analysis for Experiment or Study 2. |
| `resistance cold` | `bestiary` | The creature, its resistance wording and conditions, and its edition when identified. |

For paper cohorts, a single supported stage yields a result. The missing stage
has a null count and status `not_found`. If sources disagree about a stage,
its status is `ambiguous` and its citations show the competing evidence.
The other stage keeps its own result. A passage about recruitment alone,
planned enrollment, or a subgroup supplies insufficient evidence for an
experiment-wide count.

For a bestiary, the preview recognizes labeled resistance lists, numeric values
on hit-point rows, and explicit armor statements. A row containing `HP 53;
Resistances cold 8, fire 2` can return `cold 8` with the full row as evidence.
The creature's heading may be on the preceding page. Immunity, weakness, and
damage dealt by an attack describe different properties.

## Read the evidence

The top-level SHA-256 identifies the exact input bytes. Each citation includes
source text, its block, and a one-based extracted line number. PDF citations
also include the one-based physical page and a bounding box in points measured
from the top left. DOCX citations follow the document's structural order and
leave page and geometry empty.

| Top-level status | How to read it |
| --- | --- |
| `matched` | At least one result has source evidence. Check the cited passage and any conditions. |
| `no_match` | The query found no supported result in the extracted text. |
| `ambiguous` | Section queries return the matching candidates. Cohort queries preserve evidence per stage. Conflicting sample sizes, duplicate procedure headings, or competing bestiary editions return an empty result list. |

Check `coverage` for unreadable pages and extraction issues. Section lookup
uses Word heading levels or recognizable printed PDF headings. Irregular
columns, sidebars, complex tables, and unclear page breaks can hide or
misassign evidence. Image-only pages require separate OCR. The profile rules
cover selected English layouts.

## File and extraction limits

| Input | Limits |
| --- | --- |
| PDF | 384 MiB and 1,024 physical pages. Poppler reads a single snapshot in batches of 16 pages, with a 16 MiB output cap per call and a 96 MiB total layout cap. |
| DOCX | 32 MiB per file and 8 MiB per XML part read. |

A dense PDF can exceed the layout budget before the file-size limit. Exceeding
a limit or failing extraction exits nonzero. PDF characters mapped to invalid
XML controls appear as replacement characters and flag the page with
`undecodable_text`. Persistent document ingestion is tracked on the
[development roadmap](roadmap.md#open-work).
