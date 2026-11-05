//! FTS trigger correctness, query safety and a reproducible 100k-message performance fixture.
use ss_core::Id;
use ss_server::db::{
    Pool,
    search::{self, Filter},
};
use std::time::Instant;
fn filter(q: &str) -> Filter {
    Filter {
        query: search::literal_query(q).unwrap(),
        limit: 50,
        ..Default::default()
    }
}
// One fixture deliberately follows the index through its complete mutation lifecycle.
#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn search_index_tracks_edits_deletes_rebuild_and_filters() {
    let dir = tempfile::tempdir().unwrap();
    let pool = Pool::open(dir.path().join("search.db")).await.unwrap();
    let mut conn = pool.write().await;
    let author = Id::generate();
    let channel = Id::generate();
    let hidden = Id::generate();
    sqlx::query("INSERT INTO users(id,username,display_name,password_hash,kind,created_at) VALUES(?,'author','Author','unused',0,0)").bind(author).execute(&mut *conn).await.unwrap();
    for id in [channel, hidden] {
        sqlx::query("INSERT INTO channels(id,kind,name,position,created_by,created_at) VALUES(?,2,'test',0,?,0)").bind(id).bind(author).execute(&mut *conn).await.unwrap();
    }
    let message = Id::generate();
    for (id, ch, body) in [
        (message, channel, "café launch https://example.test"),
        (Id::generate(), hidden, "secret launch"),
    ] {
        sqlx::query(
            "INSERT INTO messages(id,channel_id,author_id,content,created_at) VALUES(?,?,?,?,1000)",
        )
        .bind(id)
        .bind(ch)
        .bind(author)
        .bind(body)
        .execute(&mut *conn)
        .await
        .unwrap();
    }
    let found = search::search(&mut conn, &filter("cafe"), &[channel])
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert!(found[0].excerpt.contains("\u{1}café\u{2}"));
    assert_eq!(
        search::search(&mut conn, &filter("launch"), &[channel])
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        search::search(&mut conn, &filter("secret"), &[channel])
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        search::search(&mut conn, &filter("launch"), &[])
            .await
            .unwrap()
            .is_empty()
    );
    for q in [
        "\" OR launch*",
        "launch NEAR secret",
        "launch - secret",
        "___",
    ] {
        assert!(
            search::search(&mut conn, &filter(q), &[channel])
                .await
                .is_ok()
        );
    }
    assert!(search::literal_query("\" : ***").is_none());
    assert!(search::literal_query(&"x".repeat(257)).is_none());
    assert!(search::literal_query(&"x ".repeat(17)).is_none());
    let mut f = filter("launch");
    f.author = Some(Id::generate());
    assert!(
        search::search(&mut conn, &f, &[channel])
            .await
            .unwrap()
            .is_empty()
    );
    f.author = Some(author);
    f.before = Some(1000);
    assert!(
        search::search(&mut conn, &f, &[channel])
            .await
            .unwrap()
            .is_empty()
    );
    f.before = Some(1001);
    f.after = Some(999);
    f.has = Some("link".into());
    assert_eq!(
        search::search(&mut conn, &f, &[channel])
            .await
            .unwrap()
            .len(),
        1
    );
    f.has = Some("image".into());
    assert!(
        search::search(&mut conn, &f, &[channel])
            .await
            .unwrap()
            .is_empty()
    );
    sqlx::query("INSERT INTO attachments(id,uploader_id,message_id,filename,mime,size,sha256,storage_path,created_at) VALUES(?,?,?,'image.png','image/png',1,zeroblob(32),'fixture',0)")
        .bind(Id::generate()).bind(author).bind(message).execute(&mut *conn).await.unwrap();
    for kind in ["image", "file"] {
        f.has = Some(kind.into());
        assert_eq!(
            search::search(&mut conn, &f, &[channel])
                .await
                .unwrap()
                .len(),
            1
        );
    }
    sqlx::query("UPDATE messages SET content='changed body' WHERE id=?")
        .bind(message)
        .execute(&mut *conn)
        .await
        .unwrap();
    assert!(
        search::search(&mut conn, &filter("cafe"), &[channel])
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        search::search(&mut conn, &filter("changed"), &[channel])
            .await
            .unwrap()
            .len(),
        1
    );
    sqlx::query("UPDATE messages SET deleted_at=2000 WHERE id=?")
        .bind(message)
        .execute(&mut *conn)
        .await
        .unwrap();
    search::reindex(&mut conn).await.unwrap();
    assert!(
        search::search(&mut conn, &filter("changed"), &[channel])
            .await
            .unwrap()
            .is_empty()
    );
    sqlx::query("DELETE FROM attachments")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("DELETE FROM messages")
        .execute(&mut *conn)
        .await
        .unwrap();
    assert!(
        search::search(&mut conn, &filter("launch"), &[hidden])
            .await
            .unwrap()
            .is_empty()
    );
    // Single SQL bulk insert isolates search latency from fixture construction.
    sqlx::query("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100000) INSERT INTO messages(id,channel_id,author_id,content,created_at) SELECT randomblob(16),?,?,CASE WHEN x%1000=0 THEN 'release needle roadmap' ELSE 'ordinary conversation and discussion' END,x FROM n").bind(channel).bind(author).execute(&mut *conn).await.unwrap();
    let start = Instant::now();
    let results = search::search(&mut conn, &filter("needle roadmap"), &[channel])
        .await
        .unwrap();
    let elapsed = start.elapsed();
    assert_eq!(results.len(), 50);
    eprintln!("100k selective FTS search: {elapsed:?}");
    let start = Instant::now();
    let broad = search::search(&mut conn, &filter("conversation"), &[channel])
        .await
        .unwrap();
    eprintln!(
        "100k broad FTS search (99,900 matches): {:?}",
        start.elapsed()
    );
    assert_eq!(broad.len(), 50);
    assert!(
        broad
            .windows(2)
            .all(|pair| pair[0].created_at >= pair[1].created_at)
    );
    // Rebuild restores missing index entries and never revives deleted content.
    sqlx::query("INSERT INTO message_search(message_search) VALUES('delete-all')")
        .execute(&mut *conn)
        .await
        .unwrap();
    assert!(
        search::search(&mut conn, &filter("needle"), &[channel])
            .await
            .unwrap()
            .is_empty()
    );
    search::reindex(&mut conn).await.unwrap();
    assert_eq!(
        search::search(&mut conn, &filter("needle"), &[channel])
            .await
            .unwrap()
            .len(),
        50
    );
}

#[tokio::test]
async fn hidden_corpus_mutations_cannot_change_visible_result_order() {
    let dir = tempfile::tempdir().unwrap();
    let pool = Pool::open(dir.path().join("privacy.db")).await.unwrap();
    let mut conn = pool.write().await;
    let author = Id::generate();
    let visible = Id::generate();
    let hidden = Id::generate();
    sqlx::query("INSERT INTO users(id,username,display_name,password_hash,kind,created_at) VALUES(?,'author','Author','unused',0,0)").bind(author).execute(&mut *conn).await.unwrap();
    for channel in [visible, hidden] {
        sqlx::query("INSERT INTO channels(id,kind,name,position,created_by,created_at) VALUES(?,2,'test',0,?,0)").bind(channel).bind(author).execute(&mut *conn).await.unwrap();
    }
    let older = Id::generate();
    let newer = Id::generate();
    for (id, content, stamp) in [
        (
            older,
            "alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha beta",
            100,
        ),
        (
            newer,
            "alpha beta beta beta beta beta beta beta beta beta beta",
            200,
        ),
    ] {
        sqlx::query(
            "INSERT INTO messages(id,channel_id,author_id,content,created_at) VALUES(?,?,?,?,?)",
        )
        .bind(id)
        .bind(visible)
        .bind(author)
        .bind(content)
        .bind(stamp)
        .execute(&mut *conn)
        .await
        .unwrap();
    }
    sqlx::query("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100) INSERT INTO messages(id,channel_id,author_id,content,created_at) SELECT randomblob(16),?,?,'alpha',x FROM n")
        .bind(hidden).bind(author).execute(&mut *conn).await.unwrap();
    let query = filter("alpha beta");
    let original = search::search(&mut conn, &query, &[visible]).await.unwrap();
    let original: Vec<_> = original
        .into_iter()
        .map(|row| (row.id, row.excerpt))
        .collect();
    assert_eq!(
        original.iter().map(|row| row.0).collect::<Vec<_>>(),
        [newer, older]
    );
    // Swapping only hidden term frequencies reverses global BM25's preference
    // between these visible rows. Neither ordering nor snippets may change.
    for mutation in [
        "UPDATE messages SET content='beta' WHERE channel_id=?",
        "UPDATE messages SET deleted_at=1 WHERE channel_id=?",
        "DELETE FROM messages WHERE channel_id=?",
    ] {
        sqlx::query(mutation)
            .bind(hidden)
            .execute(&mut *conn)
            .await
            .unwrap();
        let changed = search::search(&mut conn, &query, &[visible]).await.unwrap();
        assert_eq!(
            changed
                .into_iter()
                .map(|row| (row.id, row.excerpt))
                .collect::<Vec<_>>(),
            original
        );
        let mut limited = filter("alpha beta");
        limited.limit = 1;
        let page = search::search(&mut conn, &limited, &[visible])
            .await
            .unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].id, newer);
    }
}
