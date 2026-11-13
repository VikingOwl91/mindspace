-- Empty, local-admin-only enrollment controls. No producer adoption or retrieval effects.
CREATE TABLE asset_ownership (
    asset_id TEXT PRIMARY KEY NOT NULL,
    source_id TEXT NOT NULL UNIQUE REFERENCES logical_sources(id),
    owner TEXT NOT NULL CHECK(owner='legacy'),
    epoch INTEGER NOT NULL CHECK(epoch=1),
    enrollment_location TEXT,
    operation_id TEXT NOT NULL UNIQUE,
    created_revision INTEGER NOT NULL REFERENCES m6_revisions(revision_id),
    updated_revision INTEGER NOT NULL CHECK(updated_revision=created_revision),
    UNIQUE(asset_id,source_id,operation_id,created_revision),
    FOREIGN KEY(asset_id,source_id,operation_id,created_revision)
      REFERENCES asset_control_intents(asset_id,source_id,operation_id,committed_revision)
      DEFERRABLE INITIALLY DEFERRED
);
CREATE TABLE asset_control_intents (
    operation_id TEXT PRIMARY KEY NOT NULL,
    nonce TEXT NOT NULL UNIQUE CHECK(length(nonce)=36),
    digest TEXT NOT NULL CHECK(length(digest)=64 AND digest NOT GLOB '*[^0-9a-f]*'),
    canonical_request BLOB NOT NULL CHECK(typeof(canonical_request)='blob' AND length(canonical_request)<=65536),
    request_version INTEGER NOT NULL CHECK(request_version=1),
    db_path TEXT NOT NULL,
    db_dev TEXT NOT NULL,
    db_ino TEXT NOT NULL,
    actor_kind TEXT NOT NULL CHECK(actor_kind='local-os-account@1'),
    actor_uid INTEGER NOT NULL CHECK(actor_uid>=0),
    source_id TEXT NOT NULL REFERENCES logical_sources(id),
    action TEXT NOT NULL CHECK(action='enroll_legacy@1'),
    expected_state TEXT NOT NULL CHECK(expected_state='Unmanaged'),
    expected_epoch INTEGER NOT NULL CHECK(expected_epoch=0),
    expected_revision INTEGER NOT NULL CHECK(expected_revision>0),
    base_fingerprint TEXT NOT NULL CHECK(length(base_fingerprint)=64 AND base_fingerprint NOT GLOB '*[^0-9a-f]*'),
    intended_state TEXT NOT NULL CHECK(intended_state='LegacyEnrolled'),
    owner TEXT NOT NULL CHECK(owner='legacy'),
    epoch INTEGER NOT NULL CHECK(epoch=1),
    asset_id TEXT NOT NULL UNIQUE,
    committed_revision INTEGER NOT NULL REFERENCES m6_revisions(revision_id)
      CHECK(committed_revision=expected_revision+1),
    CHECK(operation_id='local-enroll@1:'||nonce||':'||digest),
    UNIQUE(asset_id,source_id,operation_id,committed_revision),
    FOREIGN KEY(asset_id,source_id,operation_id,committed_revision)
      REFERENCES asset_ownership(asset_id,source_id,operation_id,created_revision)
      DEFERRABLE INITIALLY DEFERRED
);
CREATE TABLE asset_ownership_history (
    asset_id TEXT PRIMARY KEY NOT NULL,
    source_id TEXT NOT NULL,
    operation_id TEXT NOT NULL UNIQUE,
    owner TEXT NOT NULL CHECK(owner='legacy'),
    epoch INTEGER NOT NULL CHECK(epoch=1),
    enrollment_location TEXT,
    revision_id INTEGER NOT NULL REFERENCES m6_revisions(revision_id),
    FOREIGN KEY(asset_id,source_id,operation_id,revision_id)
      REFERENCES asset_ownership(asset_id,source_id,operation_id,created_revision)
      DEFERRABLE INITIALLY DEFERRED
);
CREATE TRIGGER m6_s3b_receipt_insert BEFORE INSERT ON asset_control_intents BEGIN
  SELECT CASE WHEN m6_write_revision() IS NULL OR NEW.committed_revision IS NOT m6_write_revision()
    OR s3b_receipt_valid(NEW.operation_id,NEW.nonce,NEW.digest,NEW.canonical_request,NEW.db_path,
       NEW.db_dev,NEW.db_ino,NEW.actor_uid,NEW.source_id,NEW.expected_revision,
       NEW.base_fingerprint,NEW.asset_id,NEW.committed_revision,NEW.request_version,NEW.actor_kind) IS NOT 1
    THEN RAISE(ABORT,'validated local administrative operation required') END;
  SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM logical_sources WHERE id=NEW.source_id AND kind='file'
    AND repository_id IS NULL AND commit_oid IS NULL AND git_path IS NULL)
    THEN RAISE(ABORT,'legacy file identity required') END;
END;
CREATE TRIGGER m6_s3b_owner_insert BEFORE INSERT ON asset_ownership BEGIN
  SELECT CASE WHEN m6_write_revision() IS NULL OR NEW.created_revision IS NOT m6_write_revision()
    OR s3b_owner_valid(NEW.asset_id,NEW.source_id,NEW.operation_id,NEW.owner,NEW.epoch,
      NEW.enrollment_location,NEW.created_revision,NEW.updated_revision) IS NOT 1
    THEN RAISE(ABORT,'validated local administrative operation required') END;
  SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM logical_sources WHERE id=NEW.source_id AND kind='file'
    AND repository_id IS NULL AND commit_oid IS NULL AND git_path IS NULL)
    THEN RAISE(ABORT,'legacy file identity required') END;
END;
CREATE TRIGGER m6_s3b_history_insert BEFORE INSERT ON asset_ownership_history BEGIN
  SELECT CASE WHEN m6_write_revision() IS NULL OR NEW.revision_id IS NOT m6_write_revision()
    OR s3b_owner_valid(NEW.asset_id,NEW.source_id,NEW.operation_id,NEW.owner,NEW.epoch,
      NEW.enrollment_location,NEW.revision_id,NEW.revision_id) IS NOT 1
    THEN RAISE(ABORT,'validated local administrative history required') END;
END;
CREATE TRIGGER m6_s3b_owner_birth AFTER INSERT ON asset_ownership BEGIN
  INSERT INTO asset_ownership_history(asset_id,source_id,operation_id,owner,epoch,enrollment_location,revision_id)
    VALUES(NEW.asset_id,NEW.source_id,NEW.operation_id,NEW.owner,NEW.epoch,NEW.enrollment_location,NEW.created_revision);
  INSERT INTO m6_row_availability(table_name,entity_key,revision_id) VALUES('asset_ownership',NEW.asset_id,NEW.created_revision);
  INSERT INTO m6_row_availability(table_name,entity_key,revision_id) VALUES('asset_ownership_history',NEW.asset_id,NEW.created_revision);
END;
CREATE TRIGGER m6_s3b_receipt_birth AFTER INSERT ON asset_control_intents BEGIN
  INSERT INTO m6_row_availability(table_name,entity_key,revision_id)
    VALUES('asset_control_intents',NEW.operation_id,NEW.committed_revision);
END;
CREATE TRIGGER m6_s3b_owner_update BEFORE UPDATE ON asset_ownership BEGIN SELECT RAISE(ABORT,'ownership enrollment is immutable'); END;
CREATE TRIGGER m6_s3b_owner_delete BEFORE DELETE ON asset_ownership BEGIN SELECT RAISE(ABORT,'ownership enrollment is immutable'); END;
CREATE TRIGGER m6_s3b_receipt_update BEFORE UPDATE ON asset_control_intents BEGIN SELECT RAISE(ABORT,'successful operation receipt is immutable'); END;
CREATE TRIGGER m6_s3b_receipt_delete BEFORE DELETE ON asset_control_intents BEGIN SELECT RAISE(ABORT,'successful operation receipt is immutable'); END;
CREATE TRIGGER m6_s3b_history_update BEFORE UPDATE ON asset_ownership_history BEGIN SELECT RAISE(ABORT,'ownership history is immutable'); END;
CREATE TRIGGER m6_s3b_history_delete BEFORE DELETE ON asset_ownership_history BEGIN SELECT RAISE(ABORT,'ownership history is immutable'); END;
CREATE TRIGGER m6_s3b_schema_birth BEFORE INSERT ON m6_row_availability
  WHEN NEW.table_name='asset_ownership_schema' BEGIN
  SELECT CASE WHEN EXISTS(SELECT 1 FROM m6_row_availability WHERE table_name='asset_ownership_schema')
    OR NEW.entity_key IS NOT 'local-enroll@1' OR m6_write_revision() IS NULL
    OR NEW.revision_id IS NOT m6_write_revision()
    THEN RAISE(ABORT,'storage-owned ownership schema cutover required') END;
END;
CREATE TRIGGER m6_s3b_availability_insert BEFORE INSERT ON m6_row_availability
  WHEN NEW.table_name IN ('asset_ownership','asset_control_intents','asset_ownership_history') BEGIN
  SELECT CASE WHEN NEW.revision_id IS NOT m6_write_revision()
    OR s3b_availability_valid(NEW.table_name,NEW.entity_key,NEW.revision_id) IS NOT 1
    THEN RAISE(ABORT,'validated local administrative availability required') END;
END;
CREATE TRIGGER m6_s3b_availability_update BEFORE UPDATE ON m6_row_availability
  WHEN OLD.table_name IN ('asset_ownership_schema','asset_ownership','asset_control_intents','asset_ownership_history')
    OR NEW.table_name IN ('asset_ownership_schema','asset_ownership','asset_control_intents','asset_ownership_history')
  BEGIN SELECT RAISE(ABORT,'ownership availability is immutable'); END;
CREATE TRIGGER m6_s3b_availability_delete BEFORE DELETE ON m6_row_availability
  WHEN OLD.table_name IN ('asset_ownership_schema','asset_ownership','asset_control_intents','asset_ownership_history')
  BEGIN SELECT RAISE(ABORT,'ownership availability is immutable'); END;

-- Enrollment freezes source kind/representation identity, not presence or location.
CREATE TRIGGER m6_s3b_source_identity BEFORE UPDATE OF kind,repository_id,commit_oid,git_path ON logical_sources
  WHEN EXISTS(SELECT 1 FROM asset_ownership WHERE source_id=OLD.id)
    AND (NEW.kind IS NOT 'file' OR NEW.repository_id IS NOT NULL OR NEW.commit_oid IS NOT NULL OR NEW.git_path IS NOT NULL)
  BEGIN SELECT RAISE(ABORT,'enrolled legacy source identity is immutable'); END;

-- REPLACE performs an INSERT (its implicit DELETE need not fire DELETE triggers).
CREATE TRIGGER m6_s3b_source_identity_insert BEFORE INSERT ON logical_sources
  WHEN EXISTS(SELECT 1 FROM asset_ownership WHERE source_id=NEW.id)
    AND (NEW.kind IS NOT 'file' OR NEW.repository_id IS NOT NULL OR NEW.commit_oid IS NOT NULL OR NEW.git_path IS NOT NULL)
  BEGIN SELECT RAISE(ABORT,'enrolled legacy source identity is immutable'); END;
