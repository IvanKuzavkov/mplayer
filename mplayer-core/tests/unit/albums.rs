use super::*;

#[tokio::test]
async fn complete_album_selection_is_ordered_not_paginated_and_excludes_missing_and_other_artists()
{
    let temp = TempDirectory::new();
    let store = Store::open(&temp.0.join("library.sqlite")).await.unwrap();
    let mut records = Vec::new();
    for number in (0..PAGE_SIZE + 3).rev() {
        records.push(record(
            temp.0.join(format!("{number}.flac")),
            TrackMetadata {
                title: format!("Title {:03}", PAGE_SIZE + 3 - number),
                album: "Album".into(),
                artist: "Guest artist".into(),
                album_artist: "Album artist".into(),
                disc_number: Some((number / 100 + 1) as u32),
                track_number: Some((number % 100 + 1) as u32),
                ..Default::default()
            },
        ));
    }
    records.push(record(
        temp.0.join("other.flac"),
        TrackMetadata {
            album: "Album".into(),
            artist: "Other artist".into(),
            ..Default::default()
        },
    ));
    store.seed(records).await.unwrap();
    let key = AlbumKey {
        title: "Album".into(),
        artist: "Album artist".into(),
    };
    sqlx::query("UPDATE tracks SET missing=1 WHERE track_number=2 AND disc_number=1")
        .execute(&store.pool)
        .await
        .unwrap();
    let tracks = store.album_tracks(&key).await.unwrap();
    assert_eq!(tracks.len(), PAGE_SIZE as usize + 2);
    assert!(
        tracks
            .iter()
            .all(|track| !track.missing && track.metadata.album_artist == key.artist)
    );
    assert!(tracks.windows(2).all(|pair| (
        pair[0].metadata.disc_number,
        pair[0].metadata.track_number
    ) < (
        pair[1].metadata.disc_number,
        pair[1].metadata.track_number
    )));
    assert!(
        store
            .album_tracks(&AlbumKey {
                title: "Absent".into(),
                ..key
            })
            .await
            .unwrap()
            .is_empty()
    );
    store.close().await;
}
