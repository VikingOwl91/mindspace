-- S3d.2: semantic artifacts are isolated from every active assertion/vector/search table.
CREATE TABLE doc_staged_semantic_sets (
 semantic_id TEXT PRIMARY KEY NOT NULL CHECK(typeof(semantic_id)='text' AND length(semantic_id)=64 AND semantic_id NOT GLOB '*[^0-9a-f]*'),
 lexical_stage_id TEXT NOT NULL CHECK(typeof(lexical_stage_id)='text'),
 parent_operation_id TEXT NOT NULL CHECK(typeof(parent_operation_id)='text'),
 parent_revision INTEGER NOT NULL CHECK(typeof(parent_revision)='integer' AND parent_revision>0) REFERENCES m6_revisions(revision_id),
 parent_result_digest TEXT NOT NULL CHECK(typeof(parent_result_digest)='text' AND length(parent_result_digest)=64 AND parent_result_digest NOT GLOB '*[^0-9a-f]*'),
 input_manifest TEXT NOT NULL CHECK(typeof(input_manifest)='text' AND length(input_manifest)=64 AND input_manifest NOT GLOB '*[^0-9a-f]*'),
 input_json BLOB NOT NULL CHECK(typeof(input_json)='blob' AND length(input_json) BETWEEN 1 AND 65536),
 contract_json BLOB NOT NULL CHECK(typeof(contract_json)='blob' AND length(contract_json) BETWEEN 1 AND 65536),
 pipeline_key TEXT NOT NULL CHECK(typeof(pipeline_key)='text' AND length(pipeline_key)=64 AND pipeline_key NOT GLOB '*[^0-9a-f]*'),
 model_id TEXT NOT NULL CHECK(typeof(model_id)='text' AND length(model_id) BETWEEN 1 AND 256),
 budget_identity TEXT NOT NULL CHECK(typeof(budget_identity)='text' AND length(budget_identity) BETWEEN 1 AND 2048),
 vector_count INTEGER NOT NULL CHECK(typeof(vector_count)='integer' AND vector_count BETWEEN 0 AND 8192),
 vector_bytes INTEGER NOT NULL CHECK(typeof(vector_bytes)='integer' AND vector_bytes=vector_count*3072 AND vector_bytes<=25165824),
 metadata_bytes INTEGER NOT NULL CHECK(typeof(metadata_bytes)='integer' AND metadata_bytes BETWEEN 1 AND 8388608),
 result_digest TEXT NOT NULL CHECK(typeof(result_digest)='text' AND length(result_digest)=64 AND result_digest NOT GLOB '*[^0-9a-f]*'),
 operation_id TEXT NOT NULL UNIQUE CHECK(typeof(operation_id)='text'),
 created_revision INTEGER NOT NULL CHECK(typeof(created_revision)='integer' AND created_revision>0) REFERENCES m6_revisions(revision_id),
 state TEXT NOT NULL CHECK(typeof(state)='text' AND state='verified_semantic_unpublished'),
 FOREIGN KEY(lexical_stage_id,parent_operation_id,parent_revision,parent_result_digest) REFERENCES doc_staged_lexical_sets(stage_id,operation_id,created_revision,result_digest),
 UNIQUE(semantic_id,operation_id,created_revision,result_digest),
 FOREIGN KEY(semantic_id,operation_id,created_revision,result_digest) REFERENCES doc_semantic_stage_receipts(semantic_id,operation_id,committed_revision,result_digest) DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX idx_s3d2_parent ON doc_staged_semantic_sets(lexical_stage_id);
CREATE TABLE doc_staged_vectors (
 entry_id TEXT NOT NULL CHECK(typeof(entry_id)='text') REFERENCES doc_staged_entries(entry_id),
 semantic_id TEXT NOT NULL CHECK(typeof(semantic_id)='text') REFERENCES doc_staged_semantic_sets(semantic_id),
 ordinal INTEGER NOT NULL CHECK(typeof(ordinal)='integer' AND ordinal BETWEEN 0 AND 8191),
 vector_key TEXT NOT NULL CHECK(typeof(vector_key)='text' AND length(vector_key)=64 AND vector_key NOT GLOB '*[^0-9a-f]*'),
 payload_hash TEXT NOT NULL CHECK(typeof(payload_hash)='text' AND length(payload_hash)=64 AND payload_hash NOT GLOB '*[^0-9a-f]*'),
 token_count INTEGER NOT NULL CHECK(typeof(token_count)='integer' AND token_count BETWEEN 1 AND 2048),
 encoding_digest TEXT NOT NULL CHECK(typeof(encoding_digest)='text' AND length(encoding_digest)=64 AND encoding_digest NOT GLOB '*[^0-9a-f]*'),
 dimension INTEGER NOT NULL CHECK(typeof(dimension)='integer' AND dimension=768),
 embedding BLOB NOT NULL CHECK(typeof(embedding)='blob' AND length(embedding)=dimension*4),
 vector_hash TEXT NOT NULL CHECK(typeof(vector_hash)='text' AND length(vector_hash)=64 AND vector_hash NOT GLOB '*[^0-9a-f]*'),
 PRIMARY KEY(semantic_id,entry_id), UNIQUE(semantic_id,ordinal), UNIQUE(semantic_id,vector_key)
);
CREATE INDEX idx_s3d2_entry ON doc_staged_vectors(entry_id);
CREATE TABLE doc_semantic_stage_receipts (
 operation_id TEXT PRIMARY KEY NOT NULL CHECK(typeof(operation_id)='text'),
 nonce TEXT NOT NULL UNIQUE CHECK(typeof(nonce)='text' AND length(nonce)=36),
 digest TEXT NOT NULL CHECK(typeof(digest)='text' AND length(digest)=64 AND digest NOT GLOB '*[^0-9a-f]*'),
 canonical_request BLOB NOT NULL CHECK(typeof(canonical_request)='blob' AND length(canonical_request) BETWEEN 1 AND 65536),
 intent_json BLOB NOT NULL CHECK(typeof(intent_json)='blob' AND length(intent_json) BETWEEN 1 AND 65536),
 request_version INTEGER NOT NULL CHECK(typeof(request_version)='integer' AND request_version=1),
 db_path TEXT NOT NULL CHECK(typeof(db_path)='text'), db_dev TEXT NOT NULL CHECK(typeof(db_dev)='text'), db_ino TEXT NOT NULL CHECK(typeof(db_ino)='text'),
 actor_kind TEXT NOT NULL CHECK(typeof(actor_kind)='text' AND actor_kind='local-os-account@1'), actor_uid INTEGER NOT NULL CHECK(typeof(actor_uid)='integer' AND actor_uid>=0),
 expected_revision INTEGER NOT NULL CHECK(typeof(expected_revision)='integer' AND expected_revision>0),
 semantic_id TEXT NOT NULL UNIQUE CHECK(typeof(semantic_id)='text' AND length(semantic_id)=64 AND semantic_id NOT GLOB '*[^0-9a-f]*'),
 result_digest TEXT NOT NULL CHECK(typeof(result_digest)='text' AND length(result_digest)=64 AND result_digest NOT GLOB '*[^0-9a-f]*'),
 committed_revision INTEGER NOT NULL CHECK(typeof(committed_revision)='integer' AND committed_revision=expected_revision+1) REFERENCES m6_revisions(revision_id),
 action TEXT NOT NULL CHECK(typeof(action)='text' AND action='stage_document_semantic@1'),
 expected_state TEXT NOT NULL CHECK(typeof(expected_state)='text' AND expected_state='NoSemanticArtifact'),
 intended_state TEXT NOT NULL CHECK(typeof(intended_state)='text' AND intended_state='CreateVerifiedUnpublishedSemanticSet'),
 CHECK(operation_id='local-doc-stage-semantic@1:'||nonce||':'||digest),
 UNIQUE(semantic_id,operation_id,committed_revision,result_digest),
 FOREIGN KEY(semantic_id,operation_id,committed_revision,result_digest) REFERENCES doc_staged_semantic_sets(semantic_id,operation_id,created_revision,result_digest) DEFERRABLE INITIALLY DEFERRED
);
CREATE TRIGGER m6_s3d2_set_insert BEFORE INSERT ON doc_staged_semantic_sets BEGIN
 SELECT CASE WHEN EXISTS(SELECT 1 FROM doc_staged_semantic_sets WHERE semantic_id=NEW.semantic_id OR operation_id=NEW.operation_id)
 OR NEW.created_revision IS NOT m6_write_revision()
 OR s3d2_set_valid(NEW.semantic_id,NEW.lexical_stage_id,NEW.parent_operation_id,NEW.parent_revision,NEW.parent_result_digest,NEW.input_manifest,NEW.input_json,NEW.contract_json,NEW.pipeline_key,NEW.model_id,NEW.budget_identity,NEW.vector_count,NEW.vector_bytes,NEW.metadata_bytes,NEW.result_digest,NEW.operation_id,NEW.created_revision,NEW.state) IS NOT 1
 THEN RAISE(ABORT,'validated local semantic staging required') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM doc_staged_lexical_sets p WHERE p.stage_id=NEW.lexical_stage_id AND p.operation_id=NEW.parent_operation_id AND p.created_revision=NEW.parent_revision AND p.result_digest=NEW.parent_result_digest AND p.entry_count=NEW.vector_count AND p.budget_identity=NEW.budget_identity AND p.state='verified_lexical_unpublished')
 THEN RAISE(ABORT,'exact immutable lexical parent required') END;
 SELECT CASE WHEN (SELECT COALESCE(SUM(vector_bytes+metadata_bytes),0) FROM doc_staged_semantic_sets)+NEW.vector_bytes+NEW.metadata_bytes>134217728
 THEN RAISE(ABORT,'semantic staging logical capacity exceeded') END;
END;
CREATE TRIGGER m6_s3d2_vector_insert BEFORE INSERT ON doc_staged_vectors BEGIN
 SELECT CASE WHEN EXISTS(SELECT 1 FROM doc_staged_vectors WHERE semantic_id=NEW.semantic_id AND (entry_id=NEW.entry_id OR ordinal=NEW.ordinal OR vector_key=NEW.vector_key))
 OR s3d2_vector_valid(NEW.entry_id,NEW.semantic_id,NEW.ordinal,NEW.vector_key,NEW.payload_hash,NEW.token_count,NEW.encoding_digest,NEW.dimension,NEW.embedding,NEW.vector_hash) IS NOT 1
 OR NOT EXISTS(SELECT 1 FROM doc_staged_semantic_sets s JOIN doc_staged_entries e ON e.stage_id=s.lexical_stage_id JOIN doc_chunks c ON c.id=e.chunk_id WHERE s.semantic_id=NEW.semantic_id AND s.created_revision=m6_write_revision() AND e.entry_id=NEW.entry_id AND e.ordinal=NEW.ordinal AND e.payload_hash=NEW.payload_hash AND c.token_count=NEW.token_count AND NOT EXISTS(SELECT 1 FROM doc_semantic_stage_receipts r WHERE r.semantic_id=s.semantic_id))
 THEN RAISE(ABORT,'validated exact semantic vector required') END;
END;
CREATE TRIGGER m6_s3d2_receipt_insert BEFORE INSERT ON doc_semantic_stage_receipts BEGIN
 SELECT CASE WHEN EXISTS(SELECT 1 FROM doc_semantic_stage_receipts WHERE operation_id=NEW.operation_id OR nonce=NEW.nonce OR semantic_id=NEW.semantic_id)
 OR NEW.committed_revision IS NOT m6_write_revision()
 OR s3d2_receipt_valid(NEW.operation_id,NEW.nonce,NEW.digest,NEW.canonical_request,NEW.intent_json,NEW.request_version,NEW.db_path,NEW.db_dev,NEW.db_ino,NEW.actor_kind,NEW.actor_uid,NEW.expected_revision,NEW.semantic_id,NEW.result_digest,NEW.committed_revision,NEW.action,NEW.expected_state,NEW.intended_state) IS NOT 1
 THEN RAISE(ABORT,'validated semantic receipt required') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM doc_staged_semantic_sets s WHERE s.semantic_id=NEW.semantic_id AND s.operation_id=NEW.operation_id AND s.created_revision=NEW.committed_revision AND s.result_digest=NEW.result_digest AND s.vector_count=(SELECT COUNT(*) FROM doc_staged_vectors v WHERE v.semantic_id=s.semantic_id) AND s.vector_bytes=(SELECT COALESCE(SUM(length(embedding)),0) FROM doc_staged_vectors v WHERE v.semantic_id=s.semantic_id) AND s.vector_count=(SELECT COUNT(*) FROM doc_staged_entries e WHERE e.stage_id=s.lexical_stage_id))
 OR EXISTS(SELECT 1 FROM doc_staged_vectors v JOIN doc_staged_semantic_sets s ON s.semantic_id=v.semantic_id WHERE v.semantic_id=NEW.semantic_id AND NOT EXISTS(SELECT 1 FROM doc_staged_entries e WHERE e.stage_id=s.lexical_stage_id AND e.entry_id=v.entry_id AND e.ordinal=v.ordinal AND e.payload_hash=v.payload_hash))
 OR EXISTS(SELECT 1 FROM doc_staged_semantic_sets s WHERE s.semantic_id=NEW.semantic_id AND s.vector_count>0 AND ((SELECT MIN(ordinal) FROM doc_staged_vectors WHERE semantic_id=s.semantic_id)<>0 OR (SELECT MAX(ordinal) FROM doc_staged_vectors WHERE semantic_id=s.semantic_id)<>s.vector_count-1))
 THEN RAISE(ABORT,'semantic vector set is incomplete') END;
END;
CREATE TRIGGER m6_s3d2_set_birth AFTER INSERT ON doc_staged_semantic_sets BEGIN INSERT INTO m6_row_availability VALUES('doc_staged_semantic_sets',NEW.semantic_id,NEW.created_revision); END;
CREATE TRIGGER m6_s3d2_vector_birth AFTER INSERT ON doc_staged_vectors BEGIN INSERT INTO m6_row_availability VALUES('doc_staged_vectors',NEW.semantic_id||':'||NEW.entry_id,m6_write_revision()); END;
CREATE TRIGGER m6_s3d2_receipt_birth AFTER INSERT ON doc_semantic_stage_receipts BEGIN INSERT INTO m6_row_availability VALUES('doc_semantic_stage_receipts',NEW.operation_id,NEW.committed_revision); END;
CREATE TRIGGER m6_s3d2_set_update BEFORE UPDATE ON doc_staged_semantic_sets BEGIN SELECT RAISE(ABORT,'semantic set is immutable'); END;
CREATE TRIGGER m6_s3d2_set_delete BEFORE DELETE ON doc_staged_semantic_sets BEGIN SELECT RAISE(ABORT,'semantic set is immutable'); END;
CREATE TRIGGER m6_s3d2_vector_update BEFORE UPDATE ON doc_staged_vectors BEGIN SELECT RAISE(ABORT,'semantic vector is immutable'); END;
CREATE TRIGGER m6_s3d2_vector_delete BEFORE DELETE ON doc_staged_vectors BEGIN SELECT RAISE(ABORT,'semantic vector is immutable'); END;
CREATE TRIGGER m6_s3d2_receipt_update BEFORE UPDATE ON doc_semantic_stage_receipts BEGIN SELECT RAISE(ABORT,'semantic receipt is immutable'); END;
CREATE TRIGGER m6_s3d2_receipt_delete BEFORE DELETE ON doc_semantic_stage_receipts BEGIN SELECT RAISE(ABORT,'semantic receipt is immutable'); END;
CREATE TRIGGER m6_s3d2_schema_birth BEFORE INSERT ON m6_row_availability WHEN NEW.table_name='semantic_staging_schema' BEGIN SELECT CASE WHEN EXISTS(SELECT 1 FROM m6_row_availability WHERE table_name='semantic_staging_schema') OR NEW.entity_key IS NOT 'local-doc-stage-semantic@1' OR NEW.revision_id IS NOT m6_write_revision() OR m6_write_revision() IS NULL THEN RAISE(ABORT,'storage-owned semantic staging cutover required') END; END;
CREATE TRIGGER m6_s3d2_availability_insert BEFORE INSERT ON m6_row_availability WHEN NEW.table_name IN ('doc_staged_semantic_sets','doc_staged_vectors','doc_semantic_stage_receipts') BEGIN SELECT CASE WHEN NEW.revision_id IS NOT m6_write_revision() OR s3d2_availability_valid(NEW.table_name,NEW.entity_key,NEW.revision_id) IS NOT 1 THEN RAISE(ABORT,'validated semantic availability required') END; END;
CREATE TRIGGER m6_s3d2_availability_update BEFORE UPDATE ON m6_row_availability WHEN OLD.table_name IN ('semantic_staging_schema','doc_staged_semantic_sets','doc_staged_vectors','doc_semantic_stage_receipts') OR NEW.table_name IN ('semantic_staging_schema','doc_staged_semantic_sets','doc_staged_vectors','doc_semantic_stage_receipts') BEGIN SELECT RAISE(ABORT,'semantic availability is immutable'); END;
CREATE TRIGGER m6_s3d2_availability_delete BEFORE DELETE ON m6_row_availability WHEN OLD.table_name IN ('semantic_staging_schema','doc_staged_semantic_sets','doc_staged_vectors','doc_semantic_stage_receipts') BEGIN SELECT RAISE(ABORT,'semantic availability is immutable'); END;
