use super::super::{begin_write, document_preparation, SqliteStore};
use super::*;
use std::sync::atomic::Ordering;

pub(super) fn fixture() -> (SqliteStore, LexicalStageTarget) {
    let (store, p) = document_preparation::prepared_test_fixture();
    let mut target = LexicalStageTarget {
        asset_id: p.asset_id,
        legacy_source_version: p.legacy_source_version,
        document_id: p.document_id,
        document_source_version: p.document_source_version,
        extraction_id: p.extraction_id,
        chunker_version: "heading_aware@1".into(),
        budget_identity: "test-token-budget@1".into(),
        plan_hash: String::new(),
    };
    {
        let conn = store.connection().unwrap();
        let snapshot = document_preparation::validated_staging_snapshot(&conn, &p).unwrap();
        let d = &snapshot.draft;
        let context = d
            .nodes
            .iter()
            .filter(|n| {
                n.kind == NodeKind::Section
                    && n.text_range == Some((0, d.canonical_text.len() as u64))
            })
            .map(|n| n.path.clone())
            .collect();
        let chunks = vec![PreparedChunk {
            start: 0,
            end: d.canonical_text.len() as u64,
            tokens: 42,
            context,
        }];
        target.plan_hash =
            documents::chunk_plan_hash(&target.chunker_version, &target.budget_identity, &chunks);
        let text_hash = d.text_hash();
        let structure_hash = d.structure_hash();
        let identity = ChunkIdentity {
            logical_id: p.document_id,
            draft: d,
            text_hash: &text_hash,
            structure_hash: &structure_hash,
            version: &target.chunker_version,
            budget_identity: &target.budget_identity,
        };
        let tx = begin_write(&conn, &store.revision_state).unwrap();
        tx.execute("INSERT INTO doc_chunk_sets(extraction_id,version,budget_identity,plan_hash,chunk_count) VALUES(?1,?2,?3,?4,1)",params![p.extraction_id.to_string(),target.chunker_version,target.budget_identity,target.plan_hash]).unwrap();
        for (ordinal, c) in chunks.iter().enumerate() {
            tx.execute("INSERT INTO doc_chunks(id,extraction_id,version,budget_identity,ordinal,text_start,text_end,token_count,context_json,anchor) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![documents::chunk_identity(&identity,c),p.extraction_id.to_string(),target.chunker_version,target.budget_identity,ordinal as i64,c.start as i64,c.end as i64,c.tokens as i64,serde_json::to_string(&c.context).unwrap(),documents::root_chunk_anchor(p.document_id,d,(c.start,c.end)).unwrap().to_string()]).unwrap();
        }
        tx.commit().unwrap();
    }
    (store, target)
}
#[test]
fn bytes_context_and_identity_are_exact_and_acyclic() {
    let (store, target) = fixture();
    let conn = store.connection().unwrap();
    let candidate = build(&conn, &target).unwrap();
    assert_eq!(candidate.entries.len(), 1);
    let entry = &candidate.entries[0];
    assert_eq!(
        entry.content,
        "# Snapshot\r\n\r\nOriginal e\u{301} 漢字 \u{202e}\u{200b} text.\r\n"
    );
    assert_eq!(entry.content.len() as u64, candidate.input.source_bytes_len);
    assert_eq!(entry.start, 0);
    assert_eq!(entry.end, candidate.input.source_bytes_len);
    assert_eq!(entry.payload_hash, sha256_hex(entry.content.as_bytes()));
    assert_eq!(entry.citation_state, "canonical_verified");
    assert_eq!(entry.original_bytes_state, "not_checked");
    assert_ne!(entry.entry_id, candidate.input.stage_id);
    assert_ne!(candidate.result_digest(), candidate.input.input_manifest);
    candidate.input.validate().unwrap();
    let mut same = candidate.input.clone();
    same.logical_base.push(0);
    same.preparation_revision += 1;
    assert_eq!(
        same.artifact_canonical(),
        candidate.input.artifact_canonical()
    );
    let mut changed = candidate.input.clone();
    changed.target.budget_identity.push('2');
    assert_ne!(
        changed.artifact_canonical(),
        candidate.input.artifact_canonical()
    );
    assert!(changed.validate().is_err());
    assert_eq!(build(&conn, &target).unwrap().entries, candidate.entries);
    assert!(store.revision_state.active_revision.load(Ordering::SeqCst) == 0);
}
#[test]
fn exact_chunk_key_and_plan_are_required() {
    let (store, target) = fixture();
    let conn = store.connection().unwrap();
    let mut bad = target.clone();
    bad.budget_identity.push('2');
    assert!(build(&conn, &bad).is_err());
    let mut bad = target.clone();
    bad.plan_hash = "a".repeat(64);
    assert!(build(&conn, &bad).is_err());
    let mut bad = target.clone();
    bad.chunker_version = "heading_aware@2".into();
    assert!(build(&conn, &bad).is_err());
    let mut bad = target;
    bad.document_id = Uuid::now_v7();
    assert!(build(&conn, &bad).is_err());
}
#[test]
fn shared_s1_byte_rules_refuse_utf8_splits_and_invalid_ranges() {
    let text = "e\u{301}漢🙂\r\n\u{202e}\u{200b}";
    for a in 0..=text.len() {
        for b in a..=text.len() {
            let slice = documents::canonical_slice(text, (a as u64, b as u64));
            assert_eq!(
                slice.is_ok(),
                text.is_char_boundary(a) && text.is_char_boundary(b)
            );
            if let Ok(slice) = slice {
                assert_eq!(slice.as_bytes(), &text.as_bytes()[a..b]);
            }
        }
    }
    assert!(documents::canonical_slice(text, (2, 1)).is_err());
    assert!(documents::canonical_slice(text, (0, u64::MAX)).is_err());
}
#[test]
fn authoritative_s3_codec_preserves_original_serialization() {
    let (store, target) = fixture();
    let conn = store.connection().unwrap();
    let p = PreparationTarget {
        asset_id: target.asset_id,
        legacy_source_version: target.legacy_source_version,
        document_id: target.document_id,
        document_source_version: target.document_source_version,
        extraction_id: target.extraction_id,
    };
    let snapshot = document_preparation::validated_staging_snapshot(&conn, &p).unwrap();
    let d = &snapshot.draft;
    let c = PreparedChunk {
        start: 1,
        end: 7,
        tokens: 11,
        context: vec![vec![0], vec![0, 1]],
    };
    let expected = sha256_hex(
        serde_json::json!(["heading_aware@1", "budget", [[1, 7, 11, [[0], [0, 1]]]]])
            .to_string()
            .as_bytes(),
    );
    assert_eq!(
        documents::chunk_plan_hash("heading_aware@1", "budget", std::slice::from_ref(&c)),
        expected
    );
    let expected = sha256_hex(
        serde_json::json!([
            target.document_id.to_string(),
            d.source_bytes_hash,
            d.text_hash(),
            d.detector_version,
            d.extractor_id,
            d.extractor_version,
            d.params_hash,
            d.structure_hash(),
            "heading_aware@1",
            "budget",
            1,
            7,
            [[0], [0, 1]]
        ])
        .to_string()
        .as_bytes(),
    );
    assert_eq!(
        documents::chunk_identity(
            &ChunkIdentity {
                logical_id: target.document_id,
                draft: d,
                text_hash: &d.text_hash(),
                structure_hash: &d.structure_hash(),
                version: "heading_aware@1",
                budget_identity: "budget"
            },
            &c
        ),
        expected
    );
}

/// Constructs a separately named unsealed C, including intentionally invalid S3 derivations.
/// These fixtures require no removal of production constraints.
fn alternate_set(
    store: &SqliteStore,
    base: &LexicalStageTarget,
    ranges: &[(u64, u64)],
    corrupt: &str,
) -> LexicalStageTarget {
    let mut t = base.clone();
    t.budget_identity = format!("test-{}", Uuid::now_v7());
    let conn = store.connection().unwrap();
    let p = PreparationTarget {
        asset_id: t.asset_id,
        legacy_source_version: t.legacy_source_version,
        document_id: t.document_id,
        document_source_version: t.document_source_version,
        extraction_id: t.extraction_id,
    };
    let snapshot = document_preparation::validated_staging_snapshot(&conn, &p).unwrap();
    let d = &snapshot.draft;
    let mut chunks = ranges
        .iter()
        .map(|&(start, end)| PreparedChunk {
            start,
            end,
            tokens: 5,
            context: d
                .nodes
                .iter()
                .filter(|n| {
                    n.kind == NodeKind::Section
                        && n.text_range.is_some_and(|(a, b)| a <= start && b >= end)
                })
                .map(|n| n.path.clone())
                .collect(),
        })
        .collect::<Vec<_>>();
    if corrupt == "context" {
        chunks[0].context = vec![vec![9999]];
    }
    t.plan_hash = documents::chunk_plan_hash(&t.chunker_version, &t.budget_identity, &chunks);
    let text_hash = d.text_hash();
    let structure_hash = d.structure_hash();
    let identity = ChunkIdentity {
        logical_id: t.document_id,
        draft: d,
        text_hash: &text_hash,
        structure_hash: &structure_hash,
        version: &t.chunker_version,
        budget_identity: &t.budget_identity,
    };
    let tx = begin_write(&conn, &store.revision_state).unwrap();
    tx.execute("INSERT INTO doc_chunk_sets(extraction_id,version,budget_identity,plan_hash,chunk_count) VALUES(?1,?2,?3,?4,?5)",params![t.extraction_id.to_string(),t.chunker_version,t.budget_identity,t.plan_hash,chunks.len() as i64+if corrupt=="count"{1}else{0}]).unwrap();
    for (ordinal, c) in chunks.iter().enumerate() {
        let range = if corrupt == "anchor" {
            (0, 1)
        } else {
            (c.start, c.end)
        };
        let id = if corrupt == "id" {
            "0".repeat(64)
        } else {
            documents::chunk_identity(&identity, c)
        };
        tx.execute("INSERT INTO doc_chunks(id,extraction_id,version,budget_identity,ordinal,text_start,text_end,token_count,context_json,anchor) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![id,t.extraction_id.to_string(),t.chunker_version,t.budget_identity,ordinal as i64,c.start as i64,c.end as i64,c.tokens as i64,serde_json::to_string(&c.context).unwrap(),documents::root_chunk_anchor(t.document_id,d,range).unwrap().to_string()]).unwrap();
    }
    tx.commit().unwrap();
    t
}
#[test]
fn fresh_malformed_sets_refuse_gaps_overlap_utf8_and_forged_provenance() {
    let (store, t) = fixture();
    let text = {
        let conn = store.connection().unwrap();
        build(&conn, &t).unwrap().entries[0].content.clone()
    };
    let n = text.len() as u64;
    for (ranges, corrupt) in [
        (vec![(0, 5), (6, n)], "gap"),
        (vec![(0, 7), (6, n)], "overlap"),
        (vec![(1, n)], "prefix"),
        (vec![(0, n - 1)], "suffix"),
        (vec![(0, n)], "context"),
        (vec![(0, n)], "anchor"),
        (vec![(0, n)], "count"),
        (vec![(0, n)], "id"),
    ] {
        let bad = alternate_set(&store, &t, &ranges, corrupt);
        let conn = store.connection().unwrap();
        assert!(build(&conn, &bad).is_err(), "accepted {corrupt}");
        assert!(
            build(&conn, &t).is_ok(),
            "unrelated C affected exact selection"
        );
    }
    let unicode = text.find('漢').unwrap() as u64 + 1;
    let bad = alternate_set(&store, &t, &[(0, unicode), (unicode, n)], "utf8");
    assert!(build(&store.connection().unwrap(), &bad).is_err());
    let valid = alternate_set(&store, &t, &[(0, 5), (5, n)], "valid");
    let conn = store.connection().unwrap();
    let candidate = build(&conn, &valid).unwrap();
    assert_eq!(
        candidate
            .entries
            .iter()
            .map(|e| e.content.as_str())
            .collect::<String>(),
        text
    );
}
#[test]
fn input_bounds_fail_closed_instead_of_attesting_partial_content() {
    let (store, t) = fixture();
    let conn = store.connection().unwrap();
    let original = build(&conn, &t).unwrap().input;
    for field in ["text", "entries", "metadata", "descriptor", "state"] {
        let mut bad = original.clone();
        match field {
            "text" => bad.source_bytes_len = MAX_TEXT + 1,
            "entries" => bad.entry_count = MAX_ENTRIES + 1,
            "metadata" => bad.metadata_bytes = MAX_METADATA + 1,
            "descriptor" => bad.lexical_descriptor = "shared-active-fts".into(),
            _ => bad.citation_policy = "original-authentic".into(),
        }
        bad.stage_id = sha256_hex(&bad.artifact_canonical());
        assert!(bad.validate().is_err(), "accepted {field}");
    }
}

#[cfg(feature = "embeddings")]
fn real_chunk_fixture(text: &str) -> (SqliteStore, LexicalStageTarget) {
    use crate::core::{ObservedFile, SourceKind, Ts};
    // S2 correctly refuses empty/whitespace files; S1 also permits an extractor whose
    // nonempty original bytes yield empty/whitespace canonical text. Exercise that model.
    let original = if text.trim().is_empty() {
        "original source"
    } else {
        text
    };
    let store = SqliteStore::open_in_memory().unwrap();
    let f = Uuid::now_v7();
    let fs;
    {
        let conn = store.connection().unwrap();
        let tx = begin_write(&conn, &store.revision_state).unwrap();
        tx.execute("INSERT INTO logical_sources(id,kind,state,current_location,created_at) VALUES(?1,'file','present','/measure.md',-42)",[f.to_string()]).unwrap();
        let observed = ObservedFile {
            location: "/measure.md".into(),
            content: original.into(),
            content_hash: sha256_hex(original.as_bytes()),
            title: "Measure".into(),
            media_type: "text/markdown".into(),
            size_bytes: original.len() as u64,
            evidences: vec![],
            assertions: vec![],
            memories: vec![],
        };
        fs = super::super::ensure_version(&tx, f, SourceKind::File, &observed, Ts(-42))
            .unwrap()
            .1;
        tx.commit().unwrap();
    }
    let asset_id = super::super::ownership::enroll_test_source(&store, f);
    let mut draft = crate::ingest::document::extract_document_bytes(
        "/measure.md",
        original.as_bytes(),
        Some("md"),
        &Default::default(),
        &Default::default(),
        Ts(-42),
    )
    .unwrap()
    .draft;
    if text != original {
        draft.canonical_text = text.into();
        draft.text_is_source = false;
        draft.nodes.clear();
    }
    let budget = crate::embeddings::budget::TokenBudget::from_file(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/nomic-tokenizer.json"),
        &crate::embeddings::manifest::NOMIC_V1_5,
    )
    .unwrap();
    let plan = crate::ingest::document::chunk::chunk_document(&draft, &budget).unwrap();
    let codec = plan
        .chunks
        .iter()
        .map(|c| PreparedChunk {
            start: c.start,
            end: c.end,
            tokens: c.tokens,
            context: c.context.clone(),
        })
        .collect::<Vec<_>>();
    let hash = documents::chunk_plan_hash(&plan.version, &plan.budget_identity, &codec);
    let x = documents::record_with_chunks(&store, draft, Some(plan)).unwrap();
    let p = PreparationTarget {
        asset_id,
        legacy_source_version: fs,
        document_id: x.logical_id,
        document_source_version: x.source_id,
        extraction_id: x.extraction_id,
    };
    document_preparation::enroll_test_preparation(&store, &p);
    (
        store,
        LexicalStageTarget {
            asset_id,
            legacy_source_version: fs,
            document_id: x.logical_id,
            document_source_version: x.source_id,
            extraction_id: x.extraction_id,
            chunker_version: "heading_aware@1".into(),
            budget_identity: budget.identity().into(),
            plan_hash: hash,
        },
    )
}
#[cfg(feature = "embeddings")]
#[test]
fn generated_real_chunk_sets_preserve_all_canonical_bytes() {
    for text in [
        "",
        " \r\n\t",
        "🙂漢e\u{301}\u{202e}\u{200b}\r\n".repeat(64).as_str(),
        "# A\r\n\r\n- one\r\n- 二\r\n\r\n```rs\r\nfn x() {}\r\n```\r\n## B\r\ntext\r\n",
    ] {
        let (store, t) = real_chunk_fixture(text);
        let conn = store.connection().unwrap();
        let c = build(&conn, &t).unwrap();
        assert_eq!(
            c.entries
                .iter()
                .map(|e| e.content.as_str())
                .collect::<String>()
                .as_bytes(),
            text.as_bytes()
        );
        assert_eq!(c.input.entry_count, c.entries.len() as u64);
        assert_eq!(c.input.source_bytes_len, text.len() as u64);
        for pair in c.entries.windows(2) {
            assert_eq!(pair[0].end, pair[1].start);
        }
    }
}
#[cfg(feature = "embeddings")]
#[test]
#[ignore = "release resource measurement; no inference; explicitly invoked"]
fn measure_actual_lexical_candidate_and_batched_citations() {
    use std::time::Instant;
    let fixture_bytes = std::env::var("MINDSPACE_S3D_MEASURE_BYTES")
        .ok()
        .and_then(|n| n.parse::<usize>().ok())
        .unwrap_or(262144);
    for (label,seed) in [("prose","The document preserves every exact source byte and citation.\n\n"),("cjk","文書検索の正確な引用。漢字と仮名を保持する。\n\n"),("unbroken","x"),("markdown","# Heading\r\n\r\nText e\u{301} 🙂.\r\n- one\r\n- two\r\n\r\n```rust\r\nfn main() {}\r\n```\r\n")] {
        let mut text=seed.repeat(fixture_bytes/seed.len()+1);let mut end=fixture_bytes;while !text.is_char_boundary(end){end-=1;}text.truncate(end);
        let (store,t)=real_chunk_fixture(&text);let conn=store.connection().unwrap();
        let p=PreparationTarget{asset_id:t.asset_id,legacy_source_version:t.legacy_source_version,document_id:t.document_id,document_source_version:t.document_source_version,extraction_id:t.extraction_id};
        for rep in 0..3 {
            let start=Instant::now();let candidate=build(&conn,&t).unwrap();let build_ms=start.elapsed().as_secs_f64()*1000.;
            let snapshot=document_preparation::validated_staging_snapshot(&conn,&p).unwrap();
            let start=Instant::now();let mut verified=0usize;
            for e in &candidate.entries { let quote=documents::verify_root_chunk(t.document_id,&snapshot.draft,(e.start,e.end),&e.anchor).unwrap();assert_eq!(quote,e.content);verified+=quote.len(); }
            let citations_ms=start.elapsed().as_secs_f64()*1000.;assert_eq!(verified,text.len());
            let hwm=std::fs::read_to_string("/proc/self/status").ok().and_then(|s|s.lines().find(|l|l.starts_with("VmHWM:")).map(str::to_owned));
            println!("{}",serde_json::json!({"kind":label,"rep":rep,"bytes":text.len(),"entries":candidate.entries.len(),"metadata_bytes":candidate.input.metadata_bytes,"candidate_build_ms":build_ms,"batched_citation_ms":citations_ms,"process_hwm":hwm}));
        }
    }
}

#[test]
fn historical_canonical_citations_survive_locator_and_blob_loss_without_claiming_originals() {
    let (store, t) = fixture();
    let before = {
        let conn = store.connection().unwrap();
        build(&conn, &t).unwrap()
    };
    {
        let conn = store.connection().unwrap();
        let tx = begin_write(&conn, &store.revision_state).unwrap();
        tx.execute(
            "UPDATE logical_sources SET state='absent',current_location=NULL WHERE id=?1",
            [t.document_id.to_string()],
        )
        .unwrap();
        tx.execute("DELETE FROM doc_blobs", []).unwrap();
        tx.commit().unwrap();
    }
    let conn = store.connection().unwrap();
    let after = build(&conn, &t).unwrap();
    assert_eq!(before.input.stage_id, after.input.stage_id);
    assert_eq!(before.entries, after.entries);
    assert_ne!(before.input.logical_base, after.input.logical_base);
    assert_eq!(after.entries[0].original_bytes_state, "not_checked");
}

#[test]
fn generated_utf8_partitions_have_unique_complete_source_coverage() {
    let (store, t) = fixture();
    let text = {
        let conn = store.connection().unwrap();
        build(&conn, &t).unwrap().entries[0].content.clone()
    };
    let boundaries = text
        .char_indices()
        .map(|(i, _)| i as u64)
        .chain(std::iter::once(text.len() as u64))
        .collect::<Vec<_>>();
    for stride in 1..=17 {
        let mut points = boundaries
            .iter()
            .step_by(stride)
            .copied()
            .collect::<Vec<_>>();
        if points.last() != Some(&(text.len() as u64)) {
            points.push(text.len() as u64);
        }
        let ranges = points.windows(2).map(|p| (p[0], p[1])).collect::<Vec<_>>();
        let named = alternate_set(&store, &t, &ranges, "partition");
        let conn = store.connection().unwrap();
        let c = build(&conn, &named).unwrap();
        assert_eq!(
            c.entries
                .iter()
                .map(|e| e.content.as_str())
                .collect::<String>(),
            text
        );
        let mut coverage = vec![0u8; text.len()];
        for e in &c.entries {
            for covered in &mut coverage[e.start as usize..e.end as usize] {
                *covered += 1;
            }
        }
        assert!(coverage.iter().all(|&count| count == 1));
    }
}
#[test]
fn identical_payloads_from_different_documents_cannot_share_artifact_or_entry_identity() {
    let (a, ta) = fixture();
    let (b, tb) = fixture();
    let ca = build(&a.connection().unwrap(), &ta).unwrap();
    let cb = build(&b.connection().unwrap(), &tb).unwrap();
    assert_eq!(ca.entries[0].content, cb.entries[0].content);
    assert_ne!(ca.input.stage_id, cb.input.stage_id);
    assert_ne!(ca.entries[0].entry_id, cb.entries[0].entry_id);
}
