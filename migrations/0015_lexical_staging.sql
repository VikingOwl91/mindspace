-- S3d.1: isolated, canonical lexical artifacts. No ordinary assertion/FTS/vector rows.
CREATE TABLE doc_staged_lexical_sets (
 stage_id TEXT PRIMARY KEY NOT NULL CHECK(typeof(stage_id)='text' AND length(stage_id)=64 AND stage_id NOT GLOB '*[^0-9a-f]*'),
 asset_id TEXT NOT NULL CHECK(typeof(asset_id)='text') REFERENCES document_preparation_enrollments(asset_id),
 source_id TEXT NOT NULL CHECK(typeof(source_id)='text') REFERENCES logical_sources(id),
 document_id TEXT NOT NULL CHECK(typeof(document_id)='text') REFERENCES logical_sources(id),
 preparation_operation_id TEXT NOT NULL CHECK(typeof(preparation_operation_id)='text') REFERENCES document_preparation_enrollments(operation_id),
 preparation_revision INTEGER NOT NULL REFERENCES m6_revisions(revision_id) CHECK(typeof(preparation_revision)='integer' AND preparation_revision>0),
 legacy_source_version TEXT NOT NULL CHECK(typeof(legacy_source_version)='text') REFERENCES sources(id),
 document_source_version TEXT NOT NULL CHECK(typeof(document_source_version)='text') REFERENCES sources(id),
 extraction_id TEXT NOT NULL CHECK(typeof(extraction_id)='text') REFERENCES doc_extractions(id),
 chunker_version TEXT NOT NULL CHECK(typeof(chunker_version)='text'),
 budget_identity TEXT NOT NULL CHECK(typeof(budget_identity)='text'),
 plan_hash TEXT NOT NULL CHECK(typeof(plan_hash)='text') CHECK(length(plan_hash)=64 AND plan_hash NOT GLOB '*[^0-9a-f]*'),
 input_manifest TEXT NOT NULL CHECK(typeof(input_manifest)='text') CHECK(length(input_manifest)=64 AND input_manifest NOT GLOB '*[^0-9a-f]*'),
 input_json BLOB NOT NULL CHECK(typeof(input_json)='blob' AND length(input_json) BETWEEN 1 AND 65536),
 lexical_descriptor TEXT NOT NULL CHECK(typeof(lexical_descriptor)='text'),
 citation_policy TEXT NOT NULL CHECK(typeof(citation_policy)='text'),
 entry_count INTEGER NOT NULL CHECK(typeof(entry_count)='integer' AND entry_count BETWEEN 0 AND 8192),
 source_bytes_len INTEGER NOT NULL CHECK(typeof(source_bytes_len)='integer' AND source_bytes_len BETWEEN 0 AND 16777216),
 metadata_bytes INTEGER NOT NULL CHECK(typeof(metadata_bytes)='integer' AND metadata_bytes BETWEEN 0 AND 16777216),
 result_digest TEXT NOT NULL CHECK(typeof(result_digest)='text') CHECK(length(result_digest)=64 AND result_digest NOT GLOB '*[^0-9a-f]*'),
 operation_id TEXT NOT NULL CHECK(typeof(operation_id)='text') UNIQUE,
 created_revision INTEGER NOT NULL REFERENCES m6_revisions(revision_id) CHECK(typeof(created_revision)='integer' AND created_revision>0),
 state TEXT NOT NULL CHECK(typeof(state)='text') CHECK(state='verified_lexical_unpublished'),
 token_assurance TEXT NOT NULL CHECK(typeof(token_assurance)='text') CHECK(token_assurance='not_performed_lexical'),
 CHECK(source_id<>document_id),
 FOREIGN KEY(extraction_id,chunker_version,budget_identity) REFERENCES doc_chunk_sets(extraction_id,version,budget_identity),
 UNIQUE(stage_id,operation_id,created_revision,result_digest),
 FOREIGN KEY(stage_id,operation_id,created_revision,result_digest) REFERENCES doc_stage_receipts(stage_id,operation_id,committed_revision,result_digest) DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX idx_s3d_fs ON doc_staged_lexical_sets(legacy_source_version);
CREATE INDEX idx_s3d_ds ON doc_staged_lexical_sets(document_source_version);
CREATE INDEX idx_s3d_x ON doc_staged_lexical_sets(extraction_id);
CREATE INDEX idx_s3d_c ON doc_staged_lexical_sets(extraction_id,chunker_version,budget_identity);
CREATE TABLE doc_staged_entries (
 entry_rowid INTEGER PRIMARY KEY CHECK(entry_rowid>0),
 entry_id TEXT NOT NULL UNIQUE CHECK(typeof(entry_id)='text' AND length(entry_id)=64 AND entry_id NOT GLOB '*[^0-9a-f]*'),
 stage_id TEXT NOT NULL CHECK(typeof(stage_id)='text') REFERENCES doc_staged_lexical_sets(stage_id),
 ordinal INTEGER NOT NULL CHECK(typeof(ordinal)='integer' AND ordinal>=0),
 chunk_id TEXT NOT NULL CHECK(typeof(chunk_id)='text') REFERENCES doc_chunks(id),
 text_start INTEGER NOT NULL CHECK(typeof(text_start)='integer' AND text_start>=0),
 text_end INTEGER NOT NULL CHECK(typeof(text_end)='integer' AND text_end>text_start),
 content TEXT NOT NULL CHECK(typeof(content)='text' AND octet_length(content)=text_end-text_start),
 payload_hash TEXT NOT NULL CHECK(typeof(payload_hash)='text' AND length(payload_hash)=64 AND payload_hash NOT GLOB '*[^0-9a-f]*'),
 anchor TEXT NOT NULL CHECK(typeof(anchor)='text'),
 context_json TEXT NOT NULL CHECK(typeof(context_json)='text'),
 citation_state TEXT NOT NULL CHECK(citation_state='canonical_verified'),
 original_bytes_state TEXT NOT NULL CHECK(original_bytes_state='not_checked'),
 UNIQUE(stage_id,ordinal), UNIQUE(stage_id,chunk_id)
);
CREATE INDEX idx_s3d_chunk_entry ON doc_staged_entries(chunk_id);
CREATE VIRTUAL TABLE doc_staged_fts USING fts5(content,content='doc_staged_entries',content_rowid='entry_rowid',tokenize='unicode61 remove_diacritics 1',detail=full);
CREATE TABLE doc_stage_receipts (
 operation_id TEXT PRIMARY KEY NOT NULL CHECK(typeof(operation_id)='text'),
 nonce TEXT NOT NULL CHECK(typeof(nonce)='text') UNIQUE CHECK(typeof(nonce)='text' AND length(nonce)=36),
 digest TEXT NOT NULL CHECK(typeof(digest)='text' AND length(digest)=64 AND digest NOT GLOB '*[^0-9a-f]*'),
 canonical_request BLOB NOT NULL CHECK(typeof(canonical_request)='blob' AND length(canonical_request) BETWEEN 1 AND 65536),
 intent_json BLOB NOT NULL CHECK(typeof(intent_json)='blob' AND length(intent_json) BETWEEN 1 AND 65536),
 request_version INTEGER NOT NULL CHECK(typeof(request_version)='integer' AND request_version=1),
 db_path TEXT NOT NULL CHECK(typeof(db_path)='text'), db_dev TEXT NOT NULL CHECK(typeof(db_dev)='text'), db_ino TEXT NOT NULL CHECK(typeof(db_ino)='text'),
 actor_kind TEXT NOT NULL CHECK(typeof(actor_kind)='text') CHECK(actor_kind='local-os-account@1'), actor_uid INTEGER NOT NULL CHECK(typeof(actor_uid)='integer' AND actor_uid>=0),
 expected_revision INTEGER NOT NULL CHECK(typeof(expected_revision)='integer' AND expected_revision>0),
 stage_id TEXT NOT NULL CHECK(typeof(stage_id)='text') UNIQUE,
 result_digest TEXT NOT NULL CHECK(typeof(result_digest)='text' AND length(result_digest)=64 AND result_digest NOT GLOB '*[^0-9a-f]*'),
 committed_revision INTEGER NOT NULL REFERENCES m6_revisions(revision_id) CHECK(typeof(committed_revision)='integer' AND committed_revision=expected_revision+1),
 action TEXT NOT NULL CHECK(typeof(action)='text') CHECK(action='stage_document_lexical@1'),
 expected_state TEXT NOT NULL CHECK(typeof(expected_state)='text') CHECK(expected_state='NoLexicalArtifact'),
 intended_state TEXT NOT NULL CHECK(typeof(intended_state)='text') CHECK(intended_state='CreateVerifiedUnpublishedLexicalStage'),
 CHECK(operation_id='local-doc-stage-lexical@1:'||nonce||':'||digest),
 UNIQUE(stage_id,operation_id,committed_revision,result_digest),
 FOREIGN KEY(stage_id,operation_id,committed_revision,result_digest) REFERENCES doc_staged_lexical_sets(stage_id,operation_id,created_revision,result_digest) DEFERRABLE INITIALLY DEFERRED
);
CREATE TRIGGER m6_s3d_set_insert BEFORE INSERT ON doc_staged_lexical_sets BEGIN
 SELECT CASE WHEN EXISTS(SELECT 1 FROM doc_staged_lexical_sets WHERE stage_id=NEW.stage_id OR operation_id=NEW.operation_id)
 OR NEW.created_revision IS NOT m6_write_revision()
 OR s3d_set_valid(NEW.stage_id,NEW.asset_id,NEW.source_id,NEW.document_id,NEW.preparation_operation_id,NEW.preparation_revision,NEW.legacy_source_version,NEW.document_source_version,NEW.extraction_id,NEW.chunker_version,NEW.budget_identity,NEW.plan_hash,NEW.input_manifest,NEW.input_json,NEW.lexical_descriptor,NEW.citation_policy,NEW.entry_count,NEW.source_bytes_len,NEW.metadata_bytes,NEW.result_digest,NEW.operation_id,NEW.created_revision,NEW.state,NEW.token_assurance) IS NOT 1
 THEN RAISE(ABORT,'validated local lexical staging required') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM doc_staged_lexical_sets WHERE lexical_descriptor IS NOT NEW.lexical_descriptor)
 THEN RAISE(ABORT,'incompatible staging FTS descriptor') END;
 SELECT CASE WHEN (SELECT COALESCE(SUM(source_bytes_len+metadata_bytes),0) FROM doc_staged_lexical_sets)+NEW.source_bytes_len+NEW.metadata_bytes>268435456
 THEN RAISE(ABORT,'lexical staging logical capacity exceeded') END;
 SELECT CASE WHEN NOT EXISTS(
 SELECT 1 FROM document_preparation_enrollments p
 JOIN asset_ownership a ON a.asset_id=p.asset_id AND a.source_id=p.source_id AND a.owner='legacy' AND a.epoch=1
 JOIN logical_sources f ON f.id=p.source_id AND f.kind='file' AND f.repository_id IS NULL AND f.commit_oid IS NULL AND f.git_path IS NULL
 JOIN logical_sources d ON d.id=p.document_id AND d.kind='document' AND d.repository_id IS NULL AND d.commit_oid IS NULL AND d.git_path IS NULL
 JOIN sources fs ON fs.id=NEW.legacy_source_version AND fs.logical_id=f.id AND fs.kind='file'
 JOIN sources ds ON ds.id=NEW.document_source_version AND ds.logical_id=d.id AND ds.kind='document'
 JOIN doc_extractions x ON x.id=NEW.extraction_id AND x.logical_id=d.id AND x.source_id=ds.id AND x.source_bytes_hash=fs.content_hash AND x.source_size=fs.size_bytes
 JOIN doc_chunk_sets c ON c.extraction_id=x.id AND c.version=NEW.chunker_version AND c.budget_identity=NEW.budget_identity AND c.plan_hash=NEW.plan_hash AND c.chunk_count=NEW.entry_count
 WHERE p.asset_id=NEW.asset_id AND p.source_id=NEW.source_id AND p.document_id=NEW.document_id AND p.operation_id=NEW.preparation_operation_id AND p.created_revision=NEW.preparation_revision AND octet_length(ds.content)=NEW.source_bytes_len)
 THEN RAISE(ABORT,'exact prepared Fs Ds X C identity required') END;
END;
CREATE TRIGGER m6_s3d_entry_insert BEFORE INSERT ON doc_staged_entries BEGIN
 SELECT CASE WHEN EXISTS(SELECT 1 FROM doc_staged_entries WHERE entry_id=NEW.entry_id OR (stage_id=NEW.stage_id AND (ordinal=NEW.ordinal OR chunk_id=NEW.chunk_id)) OR entry_rowid=NEW.entry_rowid)
 OR s3d_entry_valid(NEW.entry_id,NEW.stage_id,NEW.ordinal,NEW.chunk_id,NEW.text_start,NEW.text_end,NEW.content,NEW.payload_hash,NEW.anchor,NEW.context_json,NEW.citation_state,NEW.original_bytes_state) IS NOT 1
 OR NOT EXISTS(SELECT 1 FROM doc_staged_lexical_sets s JOIN doc_chunks c ON c.extraction_id=s.extraction_id AND c.version=s.chunker_version AND c.budget_identity=s.budget_identity WHERE s.stage_id=NEW.stage_id AND s.created_revision=m6_write_revision() AND c.id=NEW.chunk_id AND c.ordinal=NEW.ordinal AND c.text_start=NEW.text_start AND c.text_end=NEW.text_end AND c.anchor=NEW.anchor AND c.context_json=NEW.context_json)
 THEN RAISE(ABORT,'validated exact lexical entry required') END;
END;
CREATE TRIGGER m6_s3d_receipt_insert BEFORE INSERT ON doc_stage_receipts BEGIN
 SELECT CASE WHEN EXISTS(SELECT 1 FROM doc_stage_receipts WHERE operation_id=NEW.operation_id OR nonce=NEW.nonce OR stage_id=NEW.stage_id)
 OR NEW.committed_revision IS NOT m6_write_revision()
 OR s3d_receipt_valid(NEW.operation_id,NEW.nonce,NEW.digest,NEW.canonical_request,NEW.intent_json,NEW.request_version,NEW.db_path,NEW.db_dev,NEW.db_ino,NEW.actor_kind,NEW.actor_uid,NEW.expected_revision,NEW.stage_id,NEW.result_digest,NEW.committed_revision,NEW.action,NEW.expected_state,NEW.intended_state) IS NOT 1
 THEN RAISE(ABORT,'validated lexical receipt required') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM doc_staged_lexical_sets s WHERE s.stage_id=NEW.stage_id AND s.operation_id=NEW.operation_id AND s.created_revision=NEW.committed_revision AND s.result_digest=NEW.result_digest AND s.entry_count=(SELECT COUNT(*) FROM doc_staged_entries e WHERE e.stage_id=s.stage_id) AND s.source_bytes_len=(SELECT COALESCE(SUM(octet_length(content)),0) FROM doc_staged_entries e WHERE e.stage_id=s.stage_id))
 OR EXISTS(SELECT 1 FROM doc_staged_entries e WHERE e.stage_id=NEW.stage_id AND ((e.ordinal=0 AND e.text_start<>0) OR (e.ordinal>0 AND NOT EXISTS(SELECT 1 FROM doc_staged_entries p WHERE p.stage_id=e.stage_id AND p.ordinal=e.ordinal-1 AND p.text_end=e.text_start))))
 OR EXISTS(SELECT 1 FROM doc_staged_lexical_sets s WHERE s.stage_id=NEW.stage_id AND s.entry_count>0 AND ((SELECT MAX(ordinal) FROM doc_staged_entries e WHERE e.stage_id=s.stage_id)<>s.entry_count-1 OR (SELECT MAX(text_end) FROM doc_staged_entries e WHERE e.stage_id=s.stage_id)<>s.source_bytes_len))
 THEN RAISE(ABORT,'lexical stage is incomplete') END;
END;
CREATE TRIGGER m6_s3d_set_birth AFTER INSERT ON doc_staged_lexical_sets BEGIN INSERT INTO m6_row_availability VALUES('doc_staged_lexical_sets',NEW.stage_id,NEW.created_revision); END;
CREATE TRIGGER m6_s3d_entry_birth AFTER INSERT ON doc_staged_entries BEGIN
 INSERT INTO doc_staged_fts(rowid,content) VALUES(NEW.entry_rowid,NEW.content);
 INSERT INTO m6_row_availability VALUES('doc_staged_entries',NEW.entry_id,m6_write_revision());
END;
CREATE TRIGGER m6_s3d_receipt_birth AFTER INSERT ON doc_stage_receipts BEGIN INSERT INTO m6_row_availability VALUES('doc_stage_receipts',NEW.operation_id,NEW.committed_revision); END;
CREATE TRIGGER m6_s3d_set_update BEFORE UPDATE ON doc_staged_lexical_sets BEGIN SELECT RAISE(ABORT,'lexical stage is immutable'); END;
CREATE TRIGGER m6_s3d_set_delete BEFORE DELETE ON doc_staged_lexical_sets BEGIN SELECT RAISE(ABORT,'lexical stage is immutable'); END;
CREATE TRIGGER m6_s3d_entry_update BEFORE UPDATE ON doc_staged_entries BEGIN SELECT RAISE(ABORT,'lexical entry is immutable'); END;
CREATE TRIGGER m6_s3d_entry_delete BEFORE DELETE ON doc_staged_entries BEGIN SELECT RAISE(ABORT,'lexical entry is immutable'); END;
CREATE TRIGGER m6_s3d_receipt_update BEFORE UPDATE ON doc_stage_receipts BEGIN SELECT RAISE(ABORT,'lexical receipt is immutable'); END;
CREATE TRIGGER m6_s3d_receipt_delete BEFORE DELETE ON doc_stage_receipts BEGIN SELECT RAISE(ABORT,'lexical receipt is immutable'); END;
CREATE TRIGGER m6_s3d_schema_birth BEFORE INSERT ON m6_row_availability WHEN NEW.table_name='lexical_staging_schema' BEGIN
 SELECT CASE WHEN EXISTS(SELECT 1 FROM m6_row_availability WHERE table_name='lexical_staging_schema') OR NEW.entity_key IS NOT 'local-doc-stage-lexical@1' OR NEW.revision_id IS NOT m6_write_revision() OR m6_write_revision() IS NULL THEN RAISE(ABORT,'storage-owned lexical staging cutover required') END;
END;
CREATE TRIGGER m6_s3d_availability_insert BEFORE INSERT ON m6_row_availability WHEN NEW.table_name IN ('doc_staged_lexical_sets','doc_staged_entries','doc_stage_receipts') BEGIN SELECT CASE WHEN NEW.revision_id IS NOT m6_write_revision() OR s3d_availability_valid(NEW.table_name,NEW.entity_key,NEW.revision_id) IS NOT 1 THEN RAISE(ABORT,'validated lexical availability required') END; END;
CREATE TRIGGER m6_s3d_availability_update BEFORE UPDATE ON m6_row_availability WHEN OLD.table_name IN ('lexical_staging_schema','doc_staged_lexical_sets','doc_staged_entries','doc_stage_receipts') OR NEW.table_name IN ('lexical_staging_schema','doc_staged_lexical_sets','doc_staged_entries','doc_stage_receipts') BEGIN SELECT RAISE(ABORT,'lexical availability is immutable'); END;
CREATE TRIGGER m6_s3d_availability_delete BEFORE DELETE ON m6_row_availability WHEN OLD.table_name IN ('lexical_staging_schema','doc_staged_lexical_sets','doc_staged_entries','doc_stage_receipts') BEGIN SELECT RAISE(ABORT,'lexical availability is immutable'); END;

-- Seals apply to newly selected versions, not merely the initial S3c enrollment.
CREATE TRIGGER m6_s3d_source_insert BEFORE INSERT ON sources WHEN EXISTS(SELECT 1 FROM doc_staged_lexical_sets p JOIN sources s ON s.id=p.legacy_source_version OR s.id=p.document_source_version WHERE s.id=NEW.id OR (s.logical_id=NEW.logical_id AND s.content_hash=NEW.content_hash)) BEGIN SELECT RAISE(ABORT,'staged source cannot be replaced'); END;
CREATE TRIGGER m6_s3d_source_update BEFORE UPDATE ON sources WHEN EXISTS(SELECT 1 FROM doc_staged_lexical_sets WHERE legacy_source_version=OLD.id OR document_source_version=OLD.id) BEGIN SELECT RAISE(ABORT,'staged source is immutable'); END;
CREATE TRIGGER m6_s3d_source_delete BEFORE DELETE ON sources WHEN EXISTS(SELECT 1 FROM doc_staged_lexical_sets WHERE legacy_source_version=OLD.id OR document_source_version=OLD.id) BEGIN SELECT RAISE(ABORT,'staged source is immutable'); END;
CREATE TRIGGER m6_s3d_x_insert BEFORE INSERT ON doc_extractions WHEN EXISTS(SELECT 1 FROM doc_staged_lexical_sets p JOIN doc_extractions x ON x.id=p.extraction_id WHERE x.id=NEW.id OR (x.logical_id=NEW.logical_id AND x.source_bytes_hash=NEW.source_bytes_hash AND x.detector_version=NEW.detector_version AND x.extractor_id=NEW.extractor_id AND x.extractor_version=NEW.extractor_version AND x.params_hash=NEW.params_hash)) BEGIN SELECT RAISE(ABORT,'staged extraction cannot be replaced'); END;
CREATE TRIGGER m6_s3d_x_update BEFORE UPDATE ON doc_extractions WHEN EXISTS(SELECT 1 FROM doc_staged_lexical_sets WHERE extraction_id=OLD.id) BEGIN SELECT RAISE(ABORT,'staged extraction is immutable'); END;
CREATE TRIGGER m6_s3d_x_delete BEFORE DELETE ON doc_extractions WHEN EXISTS(SELECT 1 FROM doc_staged_lexical_sets WHERE extraction_id=OLD.id) BEGIN SELECT RAISE(ABORT,'staged extraction is immutable'); END;
CREATE TRIGGER m6_s3d_node_insert BEFORE INSERT ON doc_nodes WHEN EXISTS(SELECT 1 FROM doc_staged_lexical_sets WHERE extraction_id=NEW.extraction_id) BEGIN SELECT RAISE(ABORT,'staged structure is sealed'); END;
CREATE TRIGGER m6_s3d_node_update BEFORE UPDATE ON doc_nodes WHEN EXISTS(SELECT 1 FROM doc_staged_lexical_sets WHERE extraction_id=OLD.extraction_id OR extraction_id=NEW.extraction_id) BEGIN SELECT RAISE(ABORT,'staged structure is sealed'); END;
CREATE TRIGGER m6_s3d_node_delete BEFORE DELETE ON doc_nodes WHEN EXISTS(SELECT 1 FROM doc_staged_lexical_sets WHERE extraction_id=OLD.extraction_id) BEGIN SELECT RAISE(ABORT,'staged structure is sealed'); END;
CREATE TRIGGER m6_s3d_c_insert BEFORE INSERT ON doc_chunk_sets WHEN EXISTS(SELECT 1 FROM doc_staged_lexical_sets WHERE extraction_id=NEW.extraction_id AND chunker_version=NEW.version AND budget_identity=NEW.budget_identity) BEGIN SELECT RAISE(ABORT,'staged chunk set is sealed'); END;
CREATE TRIGGER m6_s3d_c_update BEFORE UPDATE ON doc_chunk_sets WHEN EXISTS(SELECT 1 FROM doc_staged_lexical_sets WHERE extraction_id=OLD.extraction_id AND chunker_version=OLD.version AND budget_identity=OLD.budget_identity) BEGIN SELECT RAISE(ABORT,'staged chunk set is sealed'); END;
CREATE TRIGGER m6_s3d_c_delete BEFORE DELETE ON doc_chunk_sets WHEN EXISTS(SELECT 1 FROM doc_staged_lexical_sets WHERE extraction_id=OLD.extraction_id AND chunker_version=OLD.version AND budget_identity=OLD.budget_identity) BEGIN SELECT RAISE(ABORT,'staged chunk set is sealed'); END;
CREATE TRIGGER m6_s3d_chunk_insert BEFORE INSERT ON doc_chunks WHEN EXISTS(SELECT 1 FROM doc_staged_lexical_sets WHERE extraction_id=NEW.extraction_id AND chunker_version=NEW.version AND budget_identity=NEW.budget_identity) OR EXISTS(SELECT 1 FROM doc_staged_entries WHERE chunk_id=NEW.id) BEGIN SELECT RAISE(ABORT,'staged chunk rows are sealed'); END;
CREATE TRIGGER m6_s3d_chunk_update BEFORE UPDATE ON doc_chunks WHEN EXISTS(SELECT 1 FROM doc_staged_entries WHERE chunk_id=OLD.id) BEGIN SELECT RAISE(ABORT,'staged chunk is immutable'); END;
CREATE TRIGGER m6_s3d_chunk_delete BEFORE DELETE ON doc_chunks WHEN EXISTS(SELECT 1 FROM doc_staged_entries WHERE chunk_id=OLD.id) BEGIN SELECT RAISE(ABORT,'staged chunk is immutable'); END;
