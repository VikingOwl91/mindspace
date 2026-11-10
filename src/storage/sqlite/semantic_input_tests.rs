use super::super::{begin_write, document_preparation, documents, lexical_input, SqliteStore};
use super::*;
use crate::core::{encode_f32_le, sha256_hex};
use crate::embeddings::manifest::NOMIC_V1_5;
use std::sync::atomic::Ordering;

pub(super) fn fixture_with_descriptor(descriptor: Option<&str>) -> (SqliteStore, SemanticTarget) {
    let (store, mut target) = lexical_input::chunked_test_fixture();
    let contract = StrictContract::current(NOMIC_V1_5.id).unwrap();
    {
        let conn = store.connection().unwrap();
        let snapshot = document_preparation::validated_staging_snapshot(
            &conn,
            &document_preparation::PreparationTarget {
                asset_id: target.asset_id,
                legacy_source_version: target.legacy_source_version,
                document_id: target.document_id,
                document_source_version: target.document_source_version,
                extraction_id: target.extraction_id,
            },
        )
        .unwrap();
        let draft = &snapshot.draft;
        #[cfg(feature = "embeddings")]
        let tokens = crate::embeddings::budget::TokenBudget::from_file(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/nomic-tokenizer.json"),
            &NOMIC_V1_5,
        )
        .unwrap()
        .validate_document(&draft.canonical_text)
        .unwrap();
        // This build cannot run a tokenizer. The declared provenance is used only
        // for structural storage fixtures; it is never proof of actual inference.
        #[cfg(not(feature = "embeddings"))]
        let tokens = 42;
        let context = draft
            .nodes
            .iter()
            .filter(|n| {
                n.kind == crate::core::NodeKind::Section
                    && n.text_range == Some((0, draft.canonical_text.len() as u64))
            })
            .map(|n| n.path.clone())
            .collect();
        let chunk = documents::PreparedChunk {
            start: 0,
            end: draft.canonical_text.len() as u64,
            tokens,
            context,
        };
        target.budget_identity = contract.budget_identity.clone();
        target.plan_hash = documents::chunk_plan_hash(
            &target.chunker_version,
            &target.budget_identity,
            std::slice::from_ref(&chunk),
        );
        let text_hash = draft.text_hash();
        let structure_hash = draft.structure_hash();
        let identity = documents::ChunkIdentity {
            logical_id: target.document_id,
            draft,
            text_hash: &text_hash,
            structure_hash: &structure_hash,
            version: &target.chunker_version,
            budget_identity: &target.budget_identity,
        };
        let tx = begin_write(&conn, &store.revision_state).unwrap();
        tx.execute("INSERT INTO doc_chunk_sets(extraction_id,version,budget_identity,plan_hash,chunk_count) VALUES(?1,?2,?3,?4,1)",params![target.extraction_id.to_string(),target.chunker_version,target.budget_identity,target.plan_hash]).unwrap();
        tx.execute("INSERT INTO doc_chunks(id,extraction_id,version,budget_identity,ordinal,text_start,text_end,token_count,context_json,anchor) VALUES(?1,?2,?3,?4,0,?5,?6,?7,?8,?9)",params![documents::chunk_identity(&identity,&chunk),target.extraction_id.to_string(),target.chunker_version,target.budget_identity,chunk.start as i64,chunk.end as i64,chunk.tokens as i64,serde_json::to_string(&chunk.context).unwrap(),documents::root_chunk_anchor(target.document_id,draft,(chunk.start,chunk.end)).unwrap().to_string()]).unwrap();
        tx.commit().unwrap();
    }
    let stage = if let Some(descriptor) = descriptor {
        lexical_staging::enroll_test_lexical_descriptor(&store, &target, Some(descriptor))
    } else {
        lexical_staging::enroll_test_lexical(&store, &target)
    };
    (
        store,
        SemanticTarget {
            lexical_stage_id: stage,
            model_id: contract.model_id.clone(),
            pipeline_key: contract.key(),
        },
    )
}
pub(super) fn fixture() -> (SqliteStore, SemanticTarget) {
    fixture_with_descriptor(None)
}
fn rows(source: &SemanticSource) -> Vec<SemanticVector> {
    let mut v = vec![0.; 768];
    v[0] = 1.;
    let embedding = encode_f32_le(&v).unwrap();
    source
        .entries
        .iter()
        .map(|e| SemanticVector {
            entry_id: e.lexical.entry_id.clone(),
            ordinal: e.lexical.ordinal,
            vector_key: e.vector_key.clone(),
            payload_hash: e.lexical.payload_hash.clone(),
            token_count: e.stored_token_count,
            encoding_digest: "a".repeat(64),
            dimension: 768,
            vector_hash: sha256_hex(&embedding),
            embedding: embedding.clone(),
        })
        .collect()
}
#[test]
fn syntax_validation_does_not_claim_current_model_or_encoder_availability() {
    let (store, target) = fixture();
    let conn = store.connection().unwrap();
    let mut historical = target.clone();
    historical.model_id = "historical-model@1".into();
    historical.pipeline_key = "b".repeat(64);
    historical.validate().unwrap();
    let historical = build(&conn, &historical).unwrap();
    historical.input.validate().unwrap();
    for model in ["", "bad model", "../bad\nmodel", "\u{202e}model"] {
        let mut bad = target.clone();
        bad.model_id = model.into();
        assert!(bad.validate().is_err());
    }
    let mut bad = target.clone();
    bad.lexical_stage_id = "0".repeat(63);
    assert!(build(&conn, &bad).is_err());
    let mut bad = target;
    bad.pipeline_key = "A".repeat(64);
    assert!(bad.validate().is_err());
}
#[test]
fn canonical_entries_ranges_and_source_identity_are_exact_and_stable() {
    let (store, target) = fixture();
    let conn = store.connection().unwrap();
    let source = build(&conn, &target).unwrap();
    assert_eq!(source.entries.len(), 1);
    let e = &source.entries[0].lexical;
    assert_eq!(
        e.content,
        "# Snapshot\r\n\r\nOriginal e\u{301} 漢字 \u{202e}\u{200b} text.\r\n"
    );
    assert_eq!(e.start, 0);
    assert_eq!(e.end, e.content.len() as u64);
    assert_eq!(e.payload_hash, sha256_hex(e.content.as_bytes()));
    assert_eq!(build(&conn, &target).unwrap().input, source.input);
    let mut temporal = source.input.clone();
    temporal.parent_revision += 1;
    temporal.lexical.logical_base.push(0);
    temporal.lexical.preparation_revision += 1;
    temporal.validate().unwrap();
    assert_eq!(
        temporal.artifact_canonical(),
        source.input.artifact_canonical()
    );
    let mut new_pipeline = target.clone();
    new_pipeline.pipeline_key = "b".repeat(64);
    let other = build(&conn, &new_pipeline).unwrap();
    assert_ne!(other.input.semantic_id, source.input.semantic_id);
    assert_ne!(other.entries[0].vector_key, source.entries[0].vector_key);
    assert_eq!(other.entries[0].lexical.entry_id, e.entry_id);
    let mut absent = target;
    absent.lexical_stage_id = "f".repeat(64);
    assert!(build(&conn, &absent).is_err());
    assert_eq!(
        store.revision_state.active_revision.load(Ordering::SeqCst),
        0
    );
}
#[test]
fn historical_parent_policy_is_pinned_without_claiming_fts_compatibility() {
    let descriptor="sqlite-fts5@1;unicode61 remove_diacritics 1;content-only;detail=full;prefix=none;sqlite=3.52.7";
    let (store, target) = fixture_with_descriptor(Some(descriptor));
    let conn = store.connection().unwrap();
    let parent = lexical_staging::canonical_parent(&conn, &target.lexical_stage_id).unwrap();
    assert_eq!(parent.candidate.input.lexical_descriptor, descriptor);
    assert_eq!(parent.candidate.input.stage_id, target.lexical_stage_id);
    let fresh = lexical_input::build(&conn, &parent.candidate.input.target).unwrap();
    assert_ne!(fresh.input.stage_id, target.lexical_stage_id);
    let source = build(&conn, &target).unwrap();
    assert_eq!(source.input.lexical.lexical_descriptor, descriptor);
    for bad in ["other-policy@1","sqlite-fts5@1;unicode61 remove_diacritics 1;content-only;detail=full;prefix=none;sqlite=4.0.0","sqlite-fts5@1;unicode61 remove_diacritics 1;content-only;detail=full;prefix=none;sqlite=3.052.7"] {
        let mut candidate=lexical_input::build(&conn,&parent.candidate.input.target).unwrap();assert!(lexical_input::pin_historical_descriptor(&mut candidate,bad).is_err());
    }
}
#[test]
fn semantic_canonical_evidence_does_not_depend_on_mutable_fts_postings() {
    let (store, target) = fixture();
    let conn = store.connection().unwrap();
    let before = build(&conn, &target).unwrap();
    conn.execute(
        "INSERT INTO doc_staged_fts(doc_staged_fts) VALUES('delete-all')",
        [],
    )
    .unwrap();
    assert!(conn
        .execute(
            "INSERT INTO doc_staged_fts(doc_staged_fts,rank) VALUES('integrity-check',1)",
            []
        )
        .is_err());
    let after = build(&conn, &target).unwrap();
    assert_eq!(before.input, after.input);
    assert_eq!(before.entries, after.entries);
}
#[test]
fn corruption_is_not_accepted_just_because_an_immutable_parent_row_exists() {
    let (store, target) = fixture();
    let conn = store.connection().unwrap();
    let source = build(&conn, &target).unwrap();
    // Simulated bit corruption bypasses one SQL seal deliberately; this is not
    // an allowed administrative mutation or a Model A same-account defense claim.
    conn.execute_batch("DROP TRIGGER m6_s3d_entry_update")
        .unwrap();
    let corrupt = "x".repeat(source.entries[0].lexical.content.len());
    conn.execute(
        "UPDATE doc_staged_entries SET content=?1,payload_hash=?2",
        params![corrupt, sha256_hex(corrupt.as_bytes())],
    )
    .unwrap();
    assert!(build(&conn, &target).is_err());
}
#[test]
fn vector_rows_require_exact_parent_membership_counts_hashes_and_normalization() {
    let (store, target) = fixture();
    let conn = store.connection().unwrap();
    let source = build(&conn, &target).unwrap();
    let contract = StrictContract::current(NOMIC_V1_5.id).unwrap();
    let valid = rows(&source);
    validate_vector_rows(&source, &contract, &valid).unwrap();
    assert!(validate_vector_rows(&source, &contract, &[]).is_err());
    let mut duplicate = valid.clone();
    duplicate.push(valid[0].clone());
    assert!(validate_vector_rows(&source, &contract, &duplicate).is_err());
    for field in [
        "entry",
        "ordinal",
        "key",
        "payload",
        "tokens",
        "encoding",
        "hash",
        "dimension",
    ] {
        let mut bad = valid.clone();
        let v = &mut bad[0];
        match field {
            "entry" => v.entry_id = "b".repeat(64),
            "ordinal" => v.ordinal += 1,
            "key" => v.vector_key = "b".repeat(64),
            "payload" => v.payload_hash = "b".repeat(64),
            "tokens" => v.token_count += 1,
            "encoding" => v.encoding_digest = "bad".into(),
            "hash" => v.vector_hash = "b".repeat(64),
            _ => v.dimension = 1,
        };
        assert!(
            validate_vector_rows(&source, &contract, &bad).is_err(),
            "accepted wrong {field}"
        );
    }
    for scalar in [0.0, 2.0, f32::NAN, f32::INFINITY] {
        let mut bad = valid.clone();
        let mut floats = vec![0f32; 768];
        floats[0] = scalar;
        bad[0].embedding = floats.iter().flat_map(|v| v.to_le_bytes()).collect();
        bad[0].vector_hash = sha256_hex(&bad[0].embedding);
        assert!(validate_vector_rows(&source, &contract, &bad).is_err());
    }
}
#[test]
fn artifact_input_is_independent_of_winning_floating_point_result() {
    let (store, target) = fixture();
    let conn = store.connection().unwrap();
    let source = build(&conn, &target).unwrap();
    let contract = StrictContract::current(NOMIC_V1_5.id).unwrap();
    let first = rows(&source);
    let mut other = first.clone();
    let mut v = vec![0f32; 768];
    v[1] = 1.;
    other[0].embedding = encode_f32_le(&v).unwrap();
    other[0].vector_hash = sha256_hex(&other[0].embedding);
    validate_vector_rows(&source, &contract, &first).unwrap();
    validate_vector_rows(&source, &contract, &other).unwrap();
    assert_ne!(
        result_digest(&source.input.semantic_id, &first),
        result_digest(&source.input.semantic_id, &other)
    );
    assert_eq!(
        build(&conn, &target).unwrap().input.semantic_id,
        source.input.semantic_id
    );
}
#[test]
fn input_resource_and_identity_metadata_fail_closed() {
    let (store, target) = fixture();
    let conn = store.connection().unwrap();
    let source = build(&conn, &target).unwrap();
    let mut invalid = source.input.clone();
    invalid.semantic_metadata_bytes += 1;
    assert!(invalid.validate().is_err());
    let mut invalid = source.input.clone();
    invalid.parent_revision = 0;
    assert!(invalid.validate().is_err());
    let mut invalid = source.input.clone();
    invalid.parent_operation_id = "forged".into();
    assert!(invalid.validate().is_err());
    let mut invalid = source.input;
    invalid.input_manifest = "b".repeat(64);
    assert!(invalid.validate().is_err());
}

#[test]
fn vector_identity_cannot_cross_document_revision_range_or_chunk_contract() {
    let (store, target) = fixture();
    let conn = store.connection().unwrap();
    let source = build(&conn, &target).unwrap();
    let input = &source.input.lexical;
    let entry = &source.entries[0].lexical;
    let original = vector_key(input, entry, &target.pipeline_key);
    for field in [
        "document",
        "revision",
        "extraction",
        "version",
        "budget",
        "plan",
    ] {
        let mut changed = input.clone();
        match field {
            "document" => changed.target.document_id = Uuid::now_v7(),
            "revision" => changed.target.document_source_version = Uuid::now_v7(),
            "extraction" => changed.target.extraction_id = Uuid::now_v7(),
            "version" => changed.target.chunker_version.push('2'),
            "budget" => changed.target.budget_identity.push('2'),
            _ => changed.target.plan_hash = "b".repeat(64),
        }
        assert_ne!(
            vector_key(&changed, entry, &target.pipeline_key),
            original,
            "unbound {field}"
        );
    }
    for field in ["entry", "chunk", "start", "end", "payload"] {
        let mut changed = entry.clone();
        match field {
            "entry" => changed.entry_id = "b".repeat(64),
            "chunk" => changed.chunk_id = "b".repeat(64),
            "start" => changed.start += 1,
            "end" => changed.end -= 1,
            _ => changed.payload_hash = "b".repeat(64),
        }
        assert_ne!(
            vector_key(input, &changed, &target.pipeline_key),
            original,
            "unbound {field}"
        );
    }
}

#[test]
fn selected_chunk_provenance_is_revalidated_before_semantic_source_construction() {
    let (store, target) = fixture();
    let conn = store.connection().unwrap();
    let source = build(&conn, &target).unwrap();
    // Deliberately remove the immutability seal to simulate corruption of C;
    // normal writers cannot make this update through the administrative API.
    conn.execute_batch("DROP TRIGGER doc_chunks_no_update")
        .unwrap();
    // The lexical parent independently seals its selected C rows.
    assert!(conn
        .execute(
            "UPDATE doc_chunks SET token_count=token_count+1 WHERE id=?1",
            [&source.entries[0].lexical.chunk_id]
        )
        .unwrap_err()
        .to_string()
        .contains("staged chunk is immutable"));
    conn.execute_batch("DROP TRIGGER m6_s3d_chunk_update")
        .unwrap();
    conn.execute(
        "UPDATE doc_chunks SET token_count=token_count+1 WHERE id=?1",
        [&source.entries[0].lexical.chunk_id],
    )
    .unwrap();
    assert!(build(&conn, &target).is_err());
}
