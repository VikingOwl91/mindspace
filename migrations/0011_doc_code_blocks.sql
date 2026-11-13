-- M8 S2: Markdown fenced/indented code blocks are structure worth citing, and the closed node-kind
-- vocabulary of migration 0010 had no word for them. SQLite cannot alter a CHECK constraint, so
-- doc_nodes is rebuilt with `code_block` added; every existing row is copied unchanged and the
-- triggers are recreated exactly. Nothing else changes.
--
-- (Migrations run with foreign keys off; no other table references doc_nodes.)

CREATE TABLE doc_nodes_new (
    extraction_id TEXT NOT NULL REFERENCES doc_extractions(id),
    path          TEXT NOT NULL CHECK (length(path) BETWEEN 1 AND 704),
    kind          TEXT NOT NULL CHECK (kind IN ('page', 'section', 'heading', 'paragraph', 'list', 'list_item',
                                                'table', 'row', 'cell', 'figure', 'caption', 'footnote',
                                                'reference', 'appendix', 'code_block')),
    text_start    INTEGER,
    text_end      INTEGER,
    page_no       INTEGER CHECK (page_no IS NULL OR page_no >= 1),
    bbox_x0       REAL,
    bbox_y0       REAL,
    bbox_x1       REAL,
    bbox_y1       REAL,
    precision     TEXT NOT NULL CHECK (precision IN ('byte_exact', 'node_exact', 'page_only', 'none')),
    PRIMARY KEY (extraction_id, path),
    CHECK ((text_start IS NULL) = (text_end IS NULL)),
    CHECK (text_start IS NULL OR (text_start >= 0 AND text_end >= text_start)),
    CHECK ((bbox_x0 IS NULL) = (bbox_y0 IS NULL) AND (bbox_y0 IS NULL) = (bbox_x1 IS NULL)
           AND (bbox_x1 IS NULL) = (bbox_y1 IS NULL)),
    CHECK (bbox_x0 IS NULL OR (page_no IS NOT NULL AND bbox_x0 <= bbox_x1 AND bbox_y0 <= bbox_y1)),
    CHECK (precision <> 'byte_exact' OR text_start IS NOT NULL),
    CHECK (precision <> 'page_only' OR (page_no IS NOT NULL AND text_start IS NULL AND bbox_x0 IS NULL)),
    CHECK (precision <> 'none' OR (page_no IS NULL AND text_start IS NULL AND bbox_x0 IS NULL))
) WITHOUT ROWID;

INSERT INTO doc_nodes_new (extraction_id, path, kind, text_start, text_end, page_no,
                           bbox_x0, bbox_y0, bbox_x1, bbox_y1, precision)
SELECT extraction_id, path, kind, text_start, text_end, page_no,
       bbox_x0, bbox_y0, bbox_x1, bbox_y1, precision
FROM doc_nodes;

DROP TABLE doc_nodes;
ALTER TABLE doc_nodes_new RENAME TO doc_nodes;

CREATE TRIGGER doc_nodes_range_inside_text BEFORE INSERT ON doc_nodes
WHEN NEW.text_end IS NOT NULL AND NEW.text_end > COALESCE(
    (SELECT length(CAST(s.content AS BLOB)) FROM doc_extractions e JOIN sources s ON s.id = e.source_id
     WHERE e.id = NEW.extraction_id), -1)
BEGIN SELECT RAISE(ABORT, 'node text range lies outside the canonical text'); END;

CREATE TRIGGER doc_nodes_append_only_u BEFORE UPDATE ON doc_nodes
BEGIN SELECT RAISE(ABORT, 'doc_nodes rows are append-only'); END;
CREATE TRIGGER doc_nodes_append_only_d BEFORE DELETE ON doc_nodes
BEGIN SELECT RAISE(ABORT, 'doc_nodes rows are append-only'); END;
