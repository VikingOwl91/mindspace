-- Preparation associations only. The immutable S3b retrieval owner stays legacy/1.
CREATE TABLE document_preparation_enrollments (
 asset_id TEXT PRIMARY KEY NOT NULL CHECK(typeof(asset_id)='text'),
 source_id TEXT NOT NULL CHECK(typeof(source_id)='text'),
 document_id TEXT NOT NULL UNIQUE REFERENCES logical_sources(id) CHECK(typeof(document_id)='text'),
 legacy_operation_id TEXT NOT NULL CHECK(typeof(legacy_operation_id)='text'),
 legacy_revision INTEGER NOT NULL CHECK(typeof(legacy_revision)='integer' AND legacy_revision>0),
 legacy_source_version TEXT NOT NULL REFERENCES sources(id) CHECK(typeof(legacy_source_version)='text'),
 document_source_version TEXT NOT NULL REFERENCES sources(id) CHECK(typeof(document_source_version)='text'),
 extraction_id TEXT NOT NULL REFERENCES doc_extractions(id) CHECK(typeof(extraction_id)='text'),
 snapshot_digest TEXT NOT NULL CHECK(length(snapshot_digest)=64 AND snapshot_digest NOT GLOB '*[^0-9a-f]*') CHECK(typeof(snapshot_digest)='text'),
 operation_id TEXT NOT NULL UNIQUE CHECK(typeof(operation_id)='text'),
 created_revision INTEGER NOT NULL REFERENCES m6_revisions(revision_id) CHECK(typeof(created_revision)='integer' AND created_revision>0),
 state TEXT NOT NULL CHECK(state='document_preparation_enrolled') CHECK(typeof(state)='text'),
 CHECK(source_id<>document_id),
 FOREIGN KEY(asset_id,source_id,legacy_operation_id,legacy_revision)
 REFERENCES asset_ownership(asset_id,source_id,operation_id,created_revision),
 UNIQUE(asset_id,source_id,document_id,legacy_source_version,document_source_version,extraction_id,snapshot_digest,operation_id,created_revision),
 FOREIGN KEY(asset_id,source_id,document_id,legacy_source_version,document_source_version,extraction_id,snapshot_digest,operation_id,created_revision)
 REFERENCES document_preparation_receipts(asset_id,source_id,document_id,legacy_source_version,document_source_version,extraction_id,snapshot_digest,operation_id,committed_revision)
 DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX idx_s3c_legacy_version ON document_preparation_enrollments(legacy_source_version);
CREATE INDEX idx_s3c_document_version ON document_preparation_enrollments(document_source_version);
CREATE INDEX idx_s3c_extraction ON document_preparation_enrollments(extraction_id);
CREATE TABLE document_preparation_receipts (
 operation_id TEXT PRIMARY KEY NOT NULL CHECK(typeof(operation_id)='text'),
 nonce TEXT NOT NULL UNIQUE CHECK(length(nonce)=36) CHECK(typeof(nonce)='text'),
 digest TEXT NOT NULL CHECK(length(digest)=64 AND digest NOT GLOB '*[^0-9a-f]*') CHECK(typeof(digest)='text'),
 canonical_request BLOB NOT NULL CHECK(typeof(canonical_request)='blob' AND length(canonical_request) BETWEEN 1 AND 65536),
 intent_json BLOB NOT NULL CHECK(typeof(intent_json)='blob' AND length(intent_json) BETWEEN 1 AND 65536),
 request_version INTEGER NOT NULL CHECK(typeof(request_version)='integer' AND request_version=1),
 db_path TEXT NOT NULL CHECK(typeof(db_path)='text'),
 db_dev TEXT NOT NULL CHECK(typeof(db_dev)='text'),
 db_ino TEXT NOT NULL CHECK(typeof(db_ino)='text'),
 actor_kind TEXT NOT NULL CHECK(actor_kind='local-os-account@1') CHECK(typeof(actor_kind)='text'),
 actor_uid INTEGER NOT NULL CHECK(typeof(actor_uid)='integer' AND actor_uid>=0),
 asset_id TEXT NOT NULL UNIQUE CHECK(typeof(asset_id)='text'),
 source_id TEXT NOT NULL REFERENCES logical_sources(id) CHECK(typeof(source_id)='text'),
 document_id TEXT NOT NULL REFERENCES logical_sources(id) CHECK(typeof(document_id)='text'),
 legacy_source_version TEXT NOT NULL REFERENCES sources(id) CHECK(typeof(legacy_source_version)='text'),
 document_source_version TEXT NOT NULL REFERENCES sources(id) CHECK(typeof(document_source_version)='text'),
 extraction_id TEXT NOT NULL REFERENCES doc_extractions(id) CHECK(typeof(extraction_id)='text'),
 snapshot_digest TEXT NOT NULL CHECK(length(snapshot_digest)=64 AND snapshot_digest NOT GLOB '*[^0-9a-f]*') CHECK(typeof(snapshot_digest)='text'),
 expected_revision INTEGER NOT NULL CHECK(typeof(expected_revision)='integer' AND expected_revision>0),
 committed_revision INTEGER NOT NULL REFERENCES m6_revisions(revision_id) CHECK(typeof(committed_revision)='integer' AND committed_revision=expected_revision+1),
 action TEXT NOT NULL CHECK(action='enroll_document_preparation@1') CHECK(typeof(action)='text'),
 expected_state TEXT NOT NULL CHECK(expected_state='NoDocumentPreparation') CHECK(typeof(expected_state)='text'),
 intended_state TEXT NOT NULL CHECK(intended_state='DocumentPreparationEnrolled') CHECK(typeof(intended_state)='text'),
 CHECK(operation_id='local-doc-prepare@1:'||nonce||':'||digest),
 UNIQUE(asset_id,source_id,document_id,legacy_source_version,document_source_version,extraction_id,snapshot_digest,operation_id,committed_revision),
 FOREIGN KEY(asset_id,source_id,document_id,legacy_source_version,document_source_version,extraction_id,snapshot_digest,operation_id,committed_revision)
 REFERENCES document_preparation_enrollments(asset_id,source_id,document_id,legacy_source_version,document_source_version,extraction_id,snapshot_digest,operation_id,created_revision)
 DEFERRABLE INITIALLY DEFERRED
);
CREATE TRIGGER m6_s3c_enrollment_insert BEFORE INSERT ON document_preparation_enrollments BEGIN
 SELECT CASE WHEN EXISTS(SELECT 1 FROM document_preparation_enrollments WHERE asset_id=NEW.asset_id OR document_id=NEW.document_id OR operation_id=NEW.operation_id)
 OR NEW.created_revision IS NOT m6_write_revision()
 OR s3c_enrollment_valid(NEW.asset_id,NEW.source_id,NEW.document_id,NEW.legacy_operation_id,NEW.legacy_revision,NEW.legacy_source_version,NEW.document_source_version,NEW.extraction_id,NEW.snapshot_digest,NEW.operation_id,NEW.created_revision,NEW.state) IS NOT 1
 THEN RAISE(ABORT,'validated local document preparation required') END;
 SELECT CASE WHEN NOT EXISTS(
 SELECT 1 FROM asset_ownership a JOIN asset_control_intents r ON r.operation_id=a.operation_id AND r.asset_id=a.asset_id AND r.source_id=a.source_id AND r.committed_revision=a.created_revision
 JOIN asset_ownership_history h ON h.asset_id=a.asset_id AND h.source_id=a.source_id AND h.operation_id=a.operation_id AND h.revision_id=a.created_revision
 JOIN logical_sources f ON f.id=a.source_id AND f.kind='file' AND f.repository_id IS NULL AND f.commit_oid IS NULL AND f.git_path IS NULL
 JOIN logical_sources d ON d.id=NEW.document_id AND d.kind='document' AND d.repository_id IS NULL AND d.commit_oid IS NULL AND d.git_path IS NULL
 JOIN sources fs ON fs.id=NEW.legacy_source_version AND fs.logical_id=f.id AND fs.kind='file'
 JOIN sources ds ON ds.id=NEW.document_source_version AND ds.logical_id=d.id AND ds.kind='document'
 JOIN doc_extractions x ON x.id=NEW.extraction_id AND x.logical_id=d.id AND x.source_id=ds.id AND x.source_bytes_hash=fs.content_hash AND x.source_size=fs.size_bytes
 WHERE a.asset_id=NEW.asset_id AND a.source_id=NEW.source_id AND a.operation_id=NEW.legacy_operation_id AND a.created_revision=NEW.legacy_revision AND a.owner='legacy' AND a.epoch=1)
 THEN RAISE(ABORT,'exact existing legacy and document extraction identities required') END;
END;
CREATE TRIGGER m6_s3c_receipt_insert BEFORE INSERT ON document_preparation_receipts BEGIN
 SELECT CASE WHEN EXISTS(SELECT 1 FROM document_preparation_receipts WHERE operation_id=NEW.operation_id OR nonce=NEW.nonce OR asset_id=NEW.asset_id)
 OR NEW.committed_revision IS NOT m6_write_revision()
 OR s3c_receipt_valid(NEW.operation_id,NEW.nonce,NEW.digest,NEW.canonical_request,NEW.intent_json,NEW.request_version,NEW.db_path,NEW.db_dev,NEW.db_ino,NEW.actor_kind,NEW.actor_uid,NEW.asset_id,NEW.source_id,NEW.document_id,NEW.legacy_source_version,NEW.document_source_version,NEW.extraction_id,NEW.snapshot_digest,NEW.expected_revision,NEW.committed_revision,NEW.action,NEW.expected_state,NEW.intended_state) IS NOT 1
 THEN RAISE(ABORT,'validated local document preparation receipt required') END;
END;
CREATE TRIGGER m6_s3c_enrollment_birth AFTER INSERT ON document_preparation_enrollments BEGIN
 INSERT INTO m6_row_availability(table_name,entity_key,revision_id) VALUES('document_preparation_enrollments',NEW.asset_id,NEW.created_revision);
END;
CREATE TRIGGER m6_s3c_receipt_birth AFTER INSERT ON document_preparation_receipts BEGIN
 INSERT INTO m6_row_availability(table_name,entity_key,revision_id) VALUES('document_preparation_receipts',NEW.operation_id,NEW.committed_revision);
END;
CREATE TRIGGER m6_s3c_enrollment_update BEFORE UPDATE ON document_preparation_enrollments BEGIN SELECT RAISE(ABORT,'document preparation enrollment is immutable'); END;
CREATE TRIGGER m6_s3c_enrollment_delete BEFORE DELETE ON document_preparation_enrollments BEGIN SELECT RAISE(ABORT,'document preparation enrollment is immutable'); END;
CREATE TRIGGER m6_s3c_receipt_update BEFORE UPDATE ON document_preparation_receipts BEGIN SELECT RAISE(ABORT,'document preparation receipt is immutable'); END;
CREATE TRIGGER m6_s3c_receipt_delete BEFORE DELETE ON document_preparation_receipts BEGIN SELECT RAISE(ABORT,'document preparation receipt is immutable'); END;
CREATE TRIGGER m6_s3c_schema_birth BEFORE INSERT ON m6_row_availability WHEN NEW.table_name='document_preparation_schema' BEGIN
 SELECT CASE WHEN EXISTS(SELECT 1 FROM m6_row_availability WHERE table_name='document_preparation_schema') OR NEW.entity_key IS NOT 'local-doc-prepare@1' OR m6_write_revision() IS NULL OR NEW.revision_id IS NOT m6_write_revision()
 THEN RAISE(ABORT,'storage-owned document preparation schema cutover required') END;
END;
CREATE TRIGGER m6_s3c_availability_insert BEFORE INSERT ON m6_row_availability WHEN NEW.table_name IN ('document_preparation_enrollments','document_preparation_receipts') BEGIN
 SELECT CASE WHEN NEW.revision_id IS NOT m6_write_revision() OR s3c_availability_valid(NEW.table_name,NEW.entity_key,NEW.revision_id) IS NOT 1 THEN RAISE(ABORT,'validated document preparation availability required') END;
END;
CREATE TRIGGER m6_s3c_availability_update BEFORE UPDATE ON m6_row_availability WHEN OLD.table_name IN ('document_preparation_schema','document_preparation_enrollments','document_preparation_receipts') OR NEW.table_name IN ('document_preparation_schema','document_preparation_enrollments','document_preparation_receipts') BEGIN SELECT RAISE(ABORT,'document preparation availability is immutable'); END;
CREATE TRIGGER m6_s3c_availability_delete BEFORE DELETE ON m6_row_availability WHEN OLD.table_name IN ('document_preparation_schema','document_preparation_enrollments','document_preparation_receipts') BEGIN SELECT RAISE(ABORT,'document preparation availability is immutable'); END;

-- Before INSERT protects REPLACE even when SQLite recursive DELETE triggers are disabled.
CREATE TRIGGER m6_s3c_source_version_insert BEFORE INSERT ON sources WHEN EXISTS(
 SELECT 1 FROM document_preparation_enrollments p JOIN sources s ON s.id=p.legacy_source_version OR s.id=p.document_source_version
 WHERE s.id=NEW.id OR (s.logical_id=NEW.logical_id AND s.content_hash=NEW.content_hash))
 BEGIN SELECT RAISE(ABORT,'prepared source version cannot be substituted'); END;
CREATE TRIGGER m6_s3c_source_version_update BEFORE UPDATE ON sources WHEN EXISTS(SELECT 1 FROM document_preparation_enrollments WHERE legacy_source_version=OLD.id OR document_source_version=OLD.id)
 BEGIN SELECT RAISE(ABORT,'prepared source version is immutable'); END;
CREATE TRIGGER m6_s3c_source_version_delete BEFORE DELETE ON sources WHEN EXISTS(SELECT 1 FROM document_preparation_enrollments WHERE legacy_source_version=OLD.id OR document_source_version=OLD.id)
 BEGIN SELECT RAISE(ABORT,'prepared source version is immutable'); END;
CREATE TRIGGER m6_s3c_extraction_insert BEFORE INSERT ON doc_extractions WHEN EXISTS(
 SELECT 1 FROM document_preparation_enrollments p JOIN doc_extractions x ON x.id=p.extraction_id
 WHERE x.id=NEW.id OR (x.logical_id=NEW.logical_id AND x.source_bytes_hash=NEW.source_bytes_hash AND x.detector_version=NEW.detector_version AND x.extractor_id=NEW.extractor_id AND x.extractor_version=NEW.extractor_version AND x.params_hash=NEW.params_hash))
 BEGIN SELECT RAISE(ABORT,'prepared extraction cannot be substituted'); END;
CREATE TRIGGER m6_s3c_extraction_update BEFORE UPDATE ON doc_extractions WHEN EXISTS(SELECT 1 FROM document_preparation_enrollments WHERE extraction_id=OLD.id)
 BEGIN SELECT RAISE(ABORT,'prepared extraction is immutable'); END;
CREATE TRIGGER m6_s3c_extraction_delete BEFORE DELETE ON doc_extractions WHEN EXISTS(SELECT 1 FROM document_preparation_enrollments WHERE extraction_id=OLD.id)
 BEGIN SELECT RAISE(ABORT,'prepared extraction is immutable'); END;
CREATE TRIGGER m6_s3c_nodes_insert BEFORE INSERT ON doc_nodes WHEN EXISTS(SELECT 1 FROM document_preparation_enrollments WHERE extraction_id=NEW.extraction_id)
 BEGIN SELECT RAISE(ABORT,'prepared extraction structure is sealed'); END;
CREATE TRIGGER m6_s3c_nodes_update BEFORE UPDATE ON doc_nodes WHEN EXISTS(SELECT 1 FROM document_preparation_enrollments WHERE extraction_id=OLD.extraction_id OR extraction_id=NEW.extraction_id)
 BEGIN SELECT RAISE(ABORT,'prepared extraction structure is sealed'); END;
CREATE TRIGGER m6_s3c_nodes_delete BEFORE DELETE ON doc_nodes WHEN EXISTS(SELECT 1 FROM document_preparation_enrollments WHERE extraction_id=OLD.extraction_id)
 BEGIN SELECT RAISE(ABORT,'prepared extraction structure is sealed'); END;
CREATE TRIGGER m6_s3c_document_insert BEFORE INSERT ON logical_sources WHEN EXISTS(
 SELECT 1 FROM document_preparation_enrollments p JOIN logical_sources d ON d.id=p.document_id
 WHERE d.id=NEW.id OR (NEW.kind='document' AND d.current_location=NEW.current_location))
 BEGIN SELECT RAISE(ABORT,'prepared document identity cannot be replaced'); END;
CREATE TRIGGER m6_s3c_document_update BEFORE UPDATE ON logical_sources WHEN
 (EXISTS(SELECT 1 FROM document_preparation_enrollments WHERE document_id=OLD.id) AND (NEW.id IS NOT OLD.id OR NEW.kind IS NOT 'document' OR NEW.repository_id IS NOT NULL OR NEW.commit_oid IS NOT NULL OR NEW.git_path IS NOT NULL))
 OR EXISTS(SELECT 1 FROM document_preparation_enrollments p JOIN logical_sources d ON d.id=p.document_id WHERE d.id<>OLD.id AND NEW.kind='document' AND d.current_location=NEW.current_location)
 BEGIN SELECT RAISE(ABORT,'prepared document representation identity is immutable'); END;
CREATE TRIGGER m6_s3c_document_delete BEFORE DELETE ON logical_sources WHEN EXISTS(SELECT 1 FROM document_preparation_enrollments WHERE document_id=OLD.id)
 BEGIN SELECT RAISE(ABORT,'prepared document identity is immutable'); END;
