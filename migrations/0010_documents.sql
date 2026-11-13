-- M8 S1: the document model's persistent foundation (docs/design-m8-document-intelligence.md 3.3,
-- docs/m8-s1-document-model.md).
--
-- Identities (four different things, never conflated):
--   document          logical_sources.id with kind = 'document' (stable across revisions at one location)
--   revision          a `sources` row of that logical source: the CANONICAL TEXT (content_hash = sha256 of it)
--   original bytes   doc_extractions.source_bytes_hash = sha256 of the file as read; may be absent from the
--                    database (doc_blobs holds them only on explicit opt-in)
--   extraction        (logical source, original-bytes hash, detector version, extractor id, extractor version,
--                    params hash):
--                    the unit that anchors and structure belong to
--
-- Documents use their own source kind so the directory lifecycle (which reasons only about kind = 'file')
-- never moves, absents or reconciles them. Nothing here touches memories, assertions or retrieval.
-- These tables are append-only (triggers below) and lie outside the M6 write fence, like scopes/views.

CREATE TABLE doc_extractions (
    id                TEXT PRIMARY KEY,
    logical_id        TEXT NOT NULL REFERENCES logical_sources(id),
    source_id         TEXT NOT NULL REFERENCES sources(id),
    source_bytes_hash TEXT NOT NULL CHECK (length(source_bytes_hash) = 64 AND source_bytes_hash NOT GLOB '*[^0-9a-f]*'),
    source_size       INTEGER NOT NULL CHECK (source_size >= 0),
    text_is_source    INTEGER NOT NULL CHECK (text_is_source IN (0, 1)),
    format            TEXT NOT NULL CHECK (length(format) BETWEEN 1 AND 32),
    format_hint       TEXT CHECK (format_hint IS NULL OR length(format_hint) BETWEEN 1 AND 32),
    detector_version  TEXT NOT NULL CHECK (length(detector_version) BETWEEN 1 AND 32),
    extractor_id      TEXT NOT NULL CHECK (length(extractor_id) BETWEEN 1 AND 64),
    extractor_version TEXT NOT NULL CHECK (length(extractor_version) BETWEEN 1 AND 32),
    params_hash       TEXT NOT NULL CHECK (length(params_hash) = 64 AND params_hash NOT GLOB '*[^0-9a-f]*'),
    structure_hash    TEXT NOT NULL CHECK (length(structure_hash) = 64 AND structure_hash NOT GLOB '*[^0-9a-f]*'),
    node_count        INTEGER NOT NULL CHECK (node_count >= 0),
    created_at        INTEGER NOT NULL,
    -- one extraction per identity (the detector that routed the bytes is part of it): an extractor that
    -- answers differently for the same identity is refused, never silently merged
    UNIQUE (logical_id, source_bytes_hash, detector_version, extractor_id, extractor_version, params_hash)
);

-- One document per location, whatever happens in application code. (An index only: no existing row is
-- touched, and only logical sources of the new kind take part.)
CREATE UNIQUE INDEX uq_logical_documents_location ON logical_sources(current_location)
    WHERE kind = 'document';
CREATE INDEX idx_doc_extractions_source ON doc_extractions(source_id);

CREATE TABLE doc_nodes (
    extraction_id TEXT NOT NULL REFERENCES doc_extractions(id),
    path          TEXT NOT NULL CHECK (length(path) BETWEEN 1 AND 704),
    kind          TEXT NOT NULL CHECK (kind IN ('page', 'section', 'heading', 'paragraph', 'list', 'list_item',
                                                'table', 'row', 'cell', 'figure', 'caption', 'footnote',
                                                'reference', 'appendix')),
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
    -- the precision a node states must be backed by what it stores: nothing is claimed that is absent
    CHECK (precision <> 'byte_exact' OR text_start IS NOT NULL),
    CHECK (precision <> 'page_only' OR (page_no IS NOT NULL AND text_start IS NULL AND bbox_x0 IS NULL)),
    CHECK (precision <> 'none' OR (page_no IS NULL AND text_start IS NULL AND bbox_x0 IS NULL))
) WITHOUT ROWID;

-- Original bytes, only on explicit opt-in. Content-addressed; the row carries no reference to a document.
-- Deletable (erasure); never updatable. The hash is verified by the writer and again by every reader.
CREATE TABLE doc_blobs (
    bytes_hash TEXT PRIMARY KEY CHECK (length(bytes_hash) = 64 AND bytes_hash NOT GLOB '*[^0-9a-f]*'),
    size       INTEGER NOT NULL CHECK (size >= 0),
    bytes      BLOB NOT NULL,
    created_at INTEGER NOT NULL,
    CHECK (length(bytes) = size)
);

-- An extraction belongs to a document source (kind = 'document') of the logical source it names.
CREATE TRIGGER doc_extractions_source_is_a_document BEFORE INSERT ON doc_extractions
WHEN NOT EXISTS (SELECT 1 FROM sources s JOIN logical_sources l ON l.id = s.logical_id
                 WHERE s.id = NEW.source_id AND s.logical_id = NEW.logical_id
                   AND s.kind = 'document' AND l.kind = 'document')
BEGIN SELECT RAISE(ABORT, 'a document extraction must reference a document source of its own logical source'); END;

-- A node's text range lies inside the canonical text of its extraction.
CREATE TRIGGER doc_nodes_range_inside_text BEFORE INSERT ON doc_nodes
WHEN NEW.text_end IS NOT NULL AND NEW.text_end > COALESCE(
    (SELECT length(CAST(s.content AS BLOB)) FROM doc_extractions e JOIN sources s ON s.id = e.source_id
     WHERE e.id = NEW.extraction_id), -1)
BEGIN SELECT RAISE(ABORT, 'node text range lies outside the canonical text'); END;

CREATE TRIGGER doc_extractions_append_only_u BEFORE UPDATE ON doc_extractions
BEGIN SELECT RAISE(ABORT, 'doc_extractions rows are append-only'); END;
CREATE TRIGGER doc_extractions_append_only_d BEFORE DELETE ON doc_extractions
BEGIN SELECT RAISE(ABORT, 'doc_extractions rows are append-only'); END;
CREATE TRIGGER doc_nodes_append_only_u BEFORE UPDATE ON doc_nodes
BEGIN SELECT RAISE(ABORT, 'doc_nodes rows are append-only'); END;
CREATE TRIGGER doc_nodes_append_only_d BEFORE DELETE ON doc_nodes
BEGIN SELECT RAISE(ABORT, 'doc_nodes rows are append-only'); END;
CREATE TRIGGER doc_blobs_no_update BEFORE UPDATE ON doc_blobs
BEGIN SELECT RAISE(ABORT, 'doc_blobs rows are never updated; erase and re-add'); END;
