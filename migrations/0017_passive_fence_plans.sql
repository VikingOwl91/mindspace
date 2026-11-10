-- Advisory plans only. No trigger is attached to an existing ordinary/shared table.
-- No owner, head, active fence, reservation or publication state is representable.
CREATE TABLE asset_fence_plans (
 plan_id TEXT PRIMARY KEY NOT NULL CHECK(typeof(plan_id)='text' AND length(plan_id)=64 AND plan_id NOT GLOB '*[^0-9a-f]*'),
 asset_id TEXT NOT NULL REFERENCES document_preparation_enrollments(asset_id) CHECK(typeof(asset_id)='text'),
 source_id TEXT NOT NULL REFERENCES logical_sources(id) CHECK(typeof(source_id)='text'),
 document_id TEXT NOT NULL REFERENCES logical_sources(id) CHECK(typeof(document_id)='text'),
 preparation_operation_id TEXT NOT NULL REFERENCES document_preparation_receipts(operation_id) CHECK(typeof(preparation_operation_id)='text'),
 preparation_revision INTEGER NOT NULL REFERENCES m6_revisions(revision_id) CHECK(typeof(preparation_revision)='integer' AND preparation_revision>0),
 legacy_source_version TEXT NOT NULL REFERENCES sources(id) CHECK(typeof(legacy_source_version)='text'),
 document_source_version TEXT NOT NULL REFERENCES sources(id) CHECK(typeof(document_source_version)='text'),
 extraction_id TEXT NOT NULL REFERENCES doc_extractions(id) CHECK(typeof(extraction_id)='text'),
 snapshot_digest TEXT NOT NULL CHECK(typeof(snapshot_digest)='text' AND length(snapshot_digest)=64 AND snapshot_digest NOT GLOB '*[^0-9a-f]*'),
 bindings_digest TEXT NOT NULL CHECK(typeof(bindings_digest)='text' AND length(bindings_digest)=64 AND bindings_digest NOT GLOB '*[^0-9a-f]*'),
 binding_count INTEGER NOT NULL CHECK(typeof(binding_count)='integer' AND binding_count BETWEEN 0 AND 128),
 expected_revision INTEGER NOT NULL CHECK(typeof(expected_revision)='integer' AND expected_revision>0),
 operation_id TEXT NOT NULL UNIQUE CHECK(typeof(operation_id)='text'),
 created_revision INTEGER NOT NULL REFERENCES m6_revisions(revision_id) CHECK(typeof(created_revision)='integer' AND created_revision=expected_revision+1),
 state TEXT NOT NULL CHECK(typeof(state)='text' AND state='passive'),
 policy TEXT NOT NULL CHECK(typeof(policy)='text' AND policy='passive-fence-plan@1'),
 CHECK(source_id<>document_id),
 UNIQUE(plan_id,created_revision),
 UNIQUE(plan_id,operation_id,created_revision),
 FOREIGN KEY(plan_id,operation_id,created_revision)
 REFERENCES asset_fence_plan_receipts(plan_id,operation_id,committed_revision) DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX idx_fence_plan_asset ON asset_fence_plans(asset_id);
CREATE TABLE asset_fence_plan_bindings (
 plan_id TEXT NOT NULL CHECK(typeof(plan_id)='text'),
 ordinal INTEGER NOT NULL CHECK(typeof(ordinal)='integer' AND ordinal BETWEEN 0 AND 127),
 namespace TEXT NOT NULL CHECK(typeof(namespace)='text' AND namespace IN ('file-absolute@1','opaque@1')),
 binding_key TEXT NOT NULL CHECK(typeof(binding_key)='text' AND length(CAST(binding_key AS BLOB)) BETWEEN 1 AND 4096 AND instr(binding_key,char(0))=0),
 witness_digest TEXT NOT NULL CHECK(typeof(witness_digest)='text' AND length(witness_digest)=64 AND witness_digest NOT GLOB '*[^0-9a-f]*'),
 witness_status TEXT NOT NULL CHECK(typeof(witness_status)='text' AND witness_status='unverified_operator_statement'),
 created_revision INTEGER NOT NULL CHECK(typeof(created_revision)='integer' AND created_revision>0),
 CHECK(namespace<>'file-absolute@1' OR (substr(binding_key,1,1)='/' AND instr(binding_key,'//')=0 AND instr(binding_key,'/./')=0 AND instr(binding_key,'/../')=0 AND substr(binding_key,-2)<>'/.' AND substr(binding_key,-3)<>'/..' AND (binding_key='/' OR substr(binding_key,-1)<>'/'))),
 PRIMARY KEY(plan_id,ordinal),
 UNIQUE(plan_id,namespace,binding_key),
 FOREIGN KEY(plan_id,created_revision) REFERENCES asset_fence_plans(plan_id,created_revision)
);
CREATE TABLE asset_fence_plan_receipts (
 operation_id TEXT PRIMARY KEY NOT NULL CHECK(typeof(operation_id)='text'),
 nonce TEXT NOT NULL UNIQUE CHECK(typeof(nonce)='text' AND length(nonce)=36),
 digest TEXT NOT NULL CHECK(typeof(digest)='text' AND length(digest)=64 AND digest NOT GLOB '*[^0-9a-f]*'),
 canonical_request BLOB NOT NULL CHECK(typeof(canonical_request)='blob' AND length(canonical_request) BETWEEN 1 AND 65536),
 intent_json TEXT NOT NULL CHECK(typeof(intent_json)='text' AND length(CAST(intent_json AS BLOB)) BETWEEN 1 AND 65536),
 request_version INTEGER NOT NULL CHECK(typeof(request_version)='integer' AND request_version=1),
 db_path TEXT NOT NULL CHECK(typeof(db_path)='text'),
 db_dev TEXT NOT NULL CHECK(typeof(db_dev)='text'),
 db_ino TEXT NOT NULL CHECK(typeof(db_ino)='text'),
 actor_kind TEXT NOT NULL CHECK(typeof(actor_kind)='text' AND actor_kind='local-os-account@1'),
 actor_uid INTEGER NOT NULL CHECK(typeof(actor_uid)='integer' AND actor_uid>=0),
 expected_revision INTEGER NOT NULL CHECK(typeof(expected_revision)='integer' AND expected_revision>0),
 plan_id TEXT NOT NULL UNIQUE CHECK(typeof(plan_id)='text'),
 committed_revision INTEGER NOT NULL REFERENCES m6_revisions(revision_id) CHECK(typeof(committed_revision)='integer' AND committed_revision=expected_revision+1),
 action TEXT NOT NULL CHECK(typeof(action)='text' AND action='prepare_fence_plan@1'),
 expected_state TEXT NOT NULL CHECK(typeof(expected_state)='text' AND expected_state='LegacyWritable'),
 intended_state TEXT NOT NULL CHECK(typeof(intended_state)='text' AND intended_state='PassivePlanOnly'),
 CHECK(operation_id='local-fence-plan@1:'||nonce||':'||digest),
 UNIQUE(plan_id,operation_id,committed_revision),
 FOREIGN KEY(plan_id,operation_id,committed_revision)
 REFERENCES asset_fence_plans(plan_id,operation_id,created_revision) DEFERRABLE INITIALLY DEFERRED
);
CREATE TRIGGER m6_s3e2_plan_insert BEFORE INSERT ON asset_fence_plans BEGIN
 SELECT CASE WHEN EXISTS(SELECT 1 FROM asset_fence_plans WHERE plan_id=NEW.plan_id OR operation_id=NEW.operation_id)
 OR NEW.created_revision IS NOT m6_write_revision()
 OR s3e2_plan_valid(NEW.plan_id,NEW.asset_id,NEW.source_id,NEW.document_id,NEW.preparation_operation_id,NEW.preparation_revision,NEW.legacy_source_version,NEW.document_source_version,NEW.extraction_id,NEW.snapshot_digest,NEW.bindings_digest,NEW.binding_count,NEW.expected_revision,NEW.operation_id,NEW.created_revision,NEW.state,NEW.policy) IS NOT 1
 THEN RAISE(ABORT,'validated passive plan intent required') END;
 SELECT CASE WHEN NOT EXISTS(
 SELECT 1 FROM document_preparation_enrollments p
 JOIN document_preparation_receipts r ON r.operation_id=p.operation_id AND r.asset_id=p.asset_id AND r.source_id=p.source_id AND r.document_id=p.document_id AND r.committed_revision=p.created_revision
 JOIN asset_ownership a ON a.asset_id=p.asset_id AND a.source_id=p.source_id AND a.owner='legacy' AND a.epoch=1
 JOIN logical_sources f ON f.id=p.source_id AND f.kind='file' AND f.repository_id IS NULL AND f.commit_oid IS NULL AND f.git_path IS NULL
 JOIN logical_sources d ON d.id=p.document_id AND d.kind='document' AND d.repository_id IS NULL AND d.commit_oid IS NULL AND d.git_path IS NULL
 JOIN sources fs ON fs.id=NEW.legacy_source_version AND fs.logical_id=f.id AND fs.kind='file'
 JOIN sources ds ON ds.id=NEW.document_source_version AND ds.logical_id=d.id AND ds.kind='document'
 JOIN doc_extractions x ON x.id=NEW.extraction_id AND x.logical_id=d.id AND x.source_id=ds.id AND x.source_bytes_hash=fs.content_hash AND x.source_size=fs.size_bytes
 WHERE p.asset_id=NEW.asset_id AND p.source_id=NEW.source_id AND p.document_id=NEW.document_id AND p.operation_id=NEW.preparation_operation_id AND p.created_revision=NEW.preparation_revision AND p.state='document_preparation_enrolled')
 THEN RAISE(ABORT,'exact existing prepared legacy/document identity required') END;
END;
CREATE TRIGGER m6_s3e2_binding_insert BEFORE INSERT ON asset_fence_plan_bindings BEGIN
 SELECT CASE WHEN EXISTS(SELECT 1 FROM asset_fence_plan_bindings WHERE plan_id=NEW.plan_id AND (ordinal=NEW.ordinal OR (namespace=NEW.namespace AND binding_key=NEW.binding_key)))
 OR NEW.created_revision IS NOT m6_write_revision()
 OR s3e2_binding_valid(NEW.plan_id,NEW.ordinal,NEW.namespace,NEW.binding_key,NEW.witness_digest,NEW.witness_status,NEW.created_revision) IS NOT 1
 OR NOT EXISTS(SELECT 1 FROM asset_fence_plans WHERE plan_id=NEW.plan_id AND created_revision=NEW.created_revision AND NEW.ordinal<binding_count AND state='passive')
 THEN RAISE(ABORT,'validated passive binding record required') END;
END;
CREATE TRIGGER m6_s3e2_receipt_insert BEFORE INSERT ON asset_fence_plan_receipts BEGIN
 SELECT CASE WHEN EXISTS(SELECT 1 FROM asset_fence_plan_receipts WHERE operation_id=NEW.operation_id OR nonce=NEW.nonce OR plan_id=NEW.plan_id)
 OR NEW.committed_revision IS NOT m6_write_revision()
 OR s3e2_receipt_valid(NEW.operation_id,NEW.nonce,NEW.digest,NEW.canonical_request,NEW.intent_json,NEW.request_version,NEW.db_path,NEW.db_dev,NEW.db_ino,NEW.actor_kind,NEW.actor_uid,NEW.expected_revision,NEW.plan_id,NEW.committed_revision,NEW.action,NEW.expected_state,NEW.intended_state) IS NOT 1
 THEN RAISE(ABORT,'validated immutable passive plan receipt required') END;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM asset_fence_plans p WHERE p.plan_id=NEW.plan_id AND p.operation_id=NEW.operation_id AND p.expected_revision=NEW.expected_revision AND p.created_revision=NEW.committed_revision AND p.state='passive' AND p.binding_count=(SELECT COUNT(*) FROM asset_fence_plan_bindings b WHERE b.plan_id=p.plan_id))
 THEN RAISE(ABORT,'complete same-operation passive plan required') END;
END;
CREATE TRIGGER m6_s3e2_plan_update BEFORE UPDATE ON asset_fence_plans BEGIN SELECT RAISE(ABORT,'passive plan is immutable'); END;
CREATE TRIGGER m6_s3e2_plan_delete BEFORE DELETE ON asset_fence_plans BEGIN SELECT RAISE(ABORT,'passive plan is immutable'); END;
CREATE TRIGGER m6_s3e2_binding_update BEFORE UPDATE ON asset_fence_plan_bindings BEGIN SELECT RAISE(ABORT,'passive binding is immutable'); END;
CREATE TRIGGER m6_s3e2_binding_delete BEFORE DELETE ON asset_fence_plan_bindings BEGIN SELECT RAISE(ABORT,'passive binding is immutable'); END;
CREATE TRIGGER m6_s3e2_receipt_update BEFORE UPDATE ON asset_fence_plan_receipts BEGIN SELECT RAISE(ABORT,'passive plan receipt is immutable'); END;
CREATE TRIGGER m6_s3e2_receipt_delete BEFORE DELETE ON asset_fence_plan_receipts BEGIN SELECT RAISE(ABORT,'passive plan receipt is immutable'); END;
