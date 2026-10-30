use super::*;

#[tokio::test]
async fn playlist_names_are_trimmed_unique_and_safe_against_concurrent_creation() {
    let temp = TempDirectory::new();
    let database = temp.0.join("library.sqlite");
    let store = Store::open(&database).await.unwrap();
    assert!(store.create_playlist(" \t ").await.is_err());
    let first = store.create_playlist("  Favourites  ").await.unwrap();
    for name in ["Favourites", " favourites ", "FAVOURITES"] {
        let error = store.create_playlist(name).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "A playlist with this name already exists."
        );
    }
    let other = Store::open(&database).await.unwrap();
    let (left, right) = tokio::join!(
        store.create_playlist("Weekend"),
        other.create_playlist(" weekend ")
    );
    assert_ne!(left.is_ok(), right.is_ok());
    let lists = store.playlists().await.unwrap();
    assert_eq!(lists.len(), 2);
    assert!(
        lists
            .iter()
            .any(|list| list.id == first && list.name == "Favourites")
    );
    store.delete_playlist(first).await.unwrap();
    store.create_playlist("favourites").await.unwrap();
    other.close().await;
    store.close().await;
    let reopened = Store::open(&database).await.unwrap();
    assert_eq!(reopened.playlists().await.unwrap().len(), 2);
    assert!(reopened.create_playlist("Favourites").await.is_err());
    reopened.close().await;
}

#[tokio::test]
async fn playlist_reordering_and_removal_preserve_duplicate_tracks_and_persist() {
    let temp = TempDirectory::new();
    let database = temp.0.join("library.sqlite");
    let store = Store::open(&database).await.unwrap();
    store
        .seed(vec![record(
            temp.0.join("one.flac"),
            TrackMetadata::default(),
        )])
        .await
        .unwrap();
    let track = store.library("", 0).await.unwrap().tracks[0].id;
    let playlist = store.create_playlist("Order").await.unwrap();
    for _ in 0..3 {
        store.add_playlist_entry(playlist, track).await.unwrap();
    }
    let original = store.playlist_entries(playlist).await.unwrap();
    let ids: Vec<_> = original.iter().map(|entry| entry.id).collect();
    store.move_playlist_entry(ids[0], 2).await.unwrap();
    let reordered = store.playlist_entries(playlist).await.unwrap();
    assert_eq!(
        reordered.iter().map(|entry| entry.id).collect::<Vec<_>>(),
        [ids[1], ids[2], ids[0]]
    );
    store.move_playlist_entry(ids[0], 0).await.unwrap();
    store.remove_playlist_entry(ids[1]).await.unwrap();
    store.close().await;
    let reopened = Store::open(&database).await.unwrap();
    let entries = reopened.playlist_entries(playlist).await.unwrap();
    assert_eq!(
        entries.iter().map(|entry| entry.id).collect::<Vec<_>>(),
        [ids[0], ids[2]]
    );
    assert!(entries.iter().all(|entry| entry.track.id == track));
    assert_eq!(reopened.library("", 0).await.unwrap().tracks.len(), 1);
    reopened.delete_playlist(playlist).await.unwrap();
    assert!(
        reopened
            .playlist_entries(playlist)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(reopened.library("", 0).await.unwrap().tracks.len(), 1);
    reopened.close().await;
}
