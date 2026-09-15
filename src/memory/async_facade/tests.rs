use super::*;
use crate::memory::engine::Mneme;

fn run_async<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime")
        .block_on(future)
}

fn rowid(outcome: InsertOutcome) -> RowId {
    match outcome {
        InsertOutcome::Inserted(id) | InsertOutcome::Merged(id) => id,
        other => panic!("期望写入,得到 {other:?}"),
    }
}

#[test]
fn async_methods_match_sync_semantics() {
    run_async(async {
        let db = Mneme::in_memory(2).expect("in_memory");
        let ns = db.namespace("t").into_async();
        let id = rowid(
            ns.insert(Record::new(vec![1.0, 0.0]).key("a"))
                .await
                .expect("insert"),
        );

        let patch = UpdatePatch {
            importance: Some(0.5),
            ..UpdatePatch::default()
        };
        assert_eq!(
            ns.update_by_rowid(id, patch).await.expect("update"),
            UpdateOutcome::Updated(id)
        );
        assert_eq!(
            ns.get_vector(id).await.expect("get_vector"),
            Some(vec![1.0, 0.0])
        );

        let other = rowid(
            ns.insert(Record::new(vec![0.0, 1.0]).key("b"))
                .await
                .expect("insert"),
        );
        ns.relate_with_options(id, other, RelateOptions::new(RelationKind::SUPPORTS, 0.5))
            .await
            .expect("relate");
        let edges = ns
            .neighbors(id, &[RelationKind::SUPPORTS])
            .await
            .expect("neighbors");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].to, other);
    });
}
