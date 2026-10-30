use super::*;
use image::ImageEncoder;

fn png(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
    let pixels = color.repeat(width as usize * height as usize);
    let mut encoded = Vec::new();
    image::codecs::png::PngEncoder::new(&mut encoded)
        .write_image(&pixels, width, height, image::ExtendedColorType::Rgba8)
        .unwrap();
    encoded
}

fn artwork(bytes: Vec<u8>) -> Artwork {
    Artwork {
        data: Arc::from(bytes),
        mime_type: "image/png".into(),
    }
}

async fn finish(cover: &mut Cover, artwork: Option<&Artwork>, context: &egui::Context) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            cover.update(artwork, context);
            if cover.pending.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("artwork worker must complete");
}

#[test]
fn decodes_real_png_and_jpeg_and_thumbnails_large_images() {
    let decoded = decode(&png(2, 3, [20, 40, 60, 255])).unwrap();
    assert_eq!(decoded.size, [2, 3]);
    assert!(
        decoded
            .pixels
            .iter()
            .all(|pixel| *pixel == egui::Color32::from_rgb(20, 40, 60))
    );
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95)
        .encode(
            &[25, 100, 200].repeat(6),
            2,
            3,
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
    let decoded = decode(&jpeg).unwrap();
    assert_eq!(decoded.size, [2, 3]);
    assert!(decoded.pixels.iter().all(|pixel| pixel.a() == 255));
    let decoded = decode(&png(2048, 512, [10, 30, 50, 255])).unwrap();
    assert_eq!(decoded.size, [1024, 256]);
}

#[test]
fn malformed_oversized_and_oversized_dimension_images_are_rejected() {
    assert!(decode(b"not an image").is_err());
    assert!(
        decode(&vec![0; MAX_ENCODED_BYTES + 1])
            .unwrap_err()
            .contains("8 MiB")
    );
    let oversized = png(MAX_SOURCE_DIMENSION + 1, 1, [0, 0, 0, 255]);
    assert!(
        decode(&oversized)
            .unwrap_err()
            .to_lowercase()
            .contains("limit")
    );
    let mut allocation_heavy = png(1, 1, [0, 0, 0, 255]);
    allocation_heavy[16..20].copy_from_slice(&4097_u32.to_be_bytes());
    allocation_heavy[20..24].copy_from_slice(&4097_u32.to_be_bytes());
    let mut crc = u32::MAX;
    for &byte in &allocation_heavy[12..29] {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320_u32.wrapping_mul(crc & 1));
        }
    }
    allocation_heavy[29..33].copy_from_slice(&(!crc).to_be_bytes());
    assert!(
        decode(&allocation_heavy)
            .unwrap_err()
            .to_lowercase()
            .contains("limit")
    );
}

#[tokio::test]
async fn same_arc_is_cached_and_bad_images_are_not_retried_each_frame() {
    let context = egui::Context::default();
    let good = artwork(png(2, 3, [255, 0, 0, 255]));
    let mut cover = Cover::default();
    finish(&mut cover, Some(&good), &context).await;
    let id = cover.texture().unwrap().id();
    for _ in 0..20 {
        cover.update(Some(&good), &context);
        assert!(cover.pending.is_none());
        assert_eq!(cover.texture().unwrap().id(), id);
    }
    let bad = artwork(b"not an image".to_vec());
    cover.update(Some(&bad), &context);
    assert_eq!(cover.texture().unwrap().id(), id);
    finish(&mut cover, Some(&bad), &context).await;
    assert!(cover.texture().is_none());
    for _ in 0..20 {
        cover.update(Some(&bad), &context);
        assert!(cover.pending.is_none());
        assert!(same_source(cover.completed.as_ref(), Some(&bad.data)));
    }
}

#[tokio::test]
async fn only_latest_desired_source_runs_after_pending_work_and_stale_result_is_not_published() {
    let context = egui::Context::default();
    let first = artwork(png(1, 1, [255, 0, 0, 255]));
    let skipped = artwork(png(2, 2, [0, 255, 0, 255]));
    let latest = artwork(png(3, 2, [0, 0, 255, 255]));
    let (sender, result) = oneshot::channel();
    let mut cover = Cover {
        source: Some(Arc::clone(&first.data)),
        texture: Some(context.load_texture(
            "old-cover",
            decode(&first.data).unwrap(),
            egui::TextureOptions::LINEAR,
        )),
        completed: None,
        pending: Some(Pending {
            source: Arc::clone(&first.data),
            result,
        }),
    };
    let displayed = cover.texture().unwrap().id();
    cover.update(Some(&skipped), &context);
    assert_eq!(cover.texture().unwrap().id(), displayed);
    cover.update(Some(&latest), &context);
    assert!(same_source(
        Some(&cover.pending.as_ref().unwrap().source),
        Some(&first.data)
    ));
    assert!(same_source(cover.source.as_ref(), Some(&latest.data)));
    sender.send(Ok(decode(&first.data).unwrap())).unwrap();
    cover.update(Some(&latest), &context);
    assert_eq!(
        cover.texture().unwrap().id(),
        displayed,
        "stale results must not replace the displayed cover"
    );
    assert!(same_source(
        Some(&cover.pending.as_ref().unwrap().source),
        Some(&latest.data)
    ));
    finish(&mut cover, Some(&latest), &context).await;
    assert_eq!(cover.texture().unwrap().size(), [3, 2]);
}

#[test]
fn clearing_artwork_during_pending_work_discards_its_result() {
    let context = egui::Context::default();
    let art = artwork(png(1, 1, [1, 2, 3, 255]));
    let (sender, result) = oneshot::channel();
    let mut cover = Cover {
        source: Some(Arc::clone(&art.data)),
        texture: None,
        completed: None,
        pending: Some(Pending {
            source: Arc::clone(&art.data),
            result,
        }),
    };
    cover.update(None, &context);
    assert!(cover.source.is_none());
    sender.send(Ok(decode(&art.data).unwrap())).unwrap();
    cover.update(None, &context);
    assert!(cover.pending.is_none());
    assert!(cover.texture().is_none());
}

#[tokio::test]
async fn identical_artwork_in_different_tracks_reuses_the_uploaded_texture() {
    let context = egui::Context::default();
    let first = artwork(png(2, 3, [20, 40, 60, 255]));
    let next = artwork(first.data.to_vec());
    assert!(!Arc::ptr_eq(&first.data, &next.data));
    let mut cover = Cover::default();
    finish(&mut cover, Some(&first), &context).await;
    let id = cover.texture().unwrap().id();
    for _ in 0..20 {
        cover.update(Some(&next), &context);
        assert_eq!(cover.texture().unwrap().id(), id);
        assert!(cover.pending.is_none());
        assert!(Arc::ptr_eq(cover.source.as_ref().unwrap(), &next.data));
        assert!(Arc::ptr_eq(cover.completed.as_ref().unwrap(), &next.data));
    }
}

#[tokio::test]
async fn replacement_cover_is_swapped_only_after_decoding_finishes() {
    let context = egui::Context::default();
    let first = artwork(png(2, 3, [20, 40, 60, 255]));
    let next = artwork(png(3, 2, [60, 40, 20, 255]));
    let mut cover = Cover::default();
    finish(&mut cover, Some(&first), &context).await;
    let id = cover.texture().unwrap().id();
    let (sender, result) = oneshot::channel();
    cover.pending = Some(Pending {
        source: Arc::clone(&next.data),
        result,
    });
    for _ in 0..20 {
        cover.update(Some(&next), &context);
        assert_eq!(cover.texture().unwrap().id(), id);
    }
    sender.send(Ok(decode(&next.data).unwrap())).unwrap();
    cover.update(Some(&next), &context);
    assert_ne!(cover.texture().unwrap().id(), id);
    assert_eq!(cover.texture().unwrap().size(), [3, 2]);
    cover.update(None, &context);
    assert!(cover.texture().is_none());
}
