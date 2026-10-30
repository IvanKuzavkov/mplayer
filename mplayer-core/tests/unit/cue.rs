use super::super::cue::parse;

#[test]
fn frame_75_carries_into_seconds_and_minutes_without_changing_tags() {
    for newline in ["\n", "\r\n"] {
        let contents = [
            "\u{feff}TITLE \"Keep 19:19:75 in this title\"",
            "FILE \"image.flac\" WAVE",
            " TRACK 01 AUDIO",
            "  INDEX 01 19:19:75",
            " TRACK 02 AUDIO",
            "\tindex\t00\t19:59:75  ",
            "  INDEX 01 20:00:75",
        ]
        .join(newline);
        let cue = parse(&contents).unwrap();
        assert_eq!(cue.title()[0], "Keep 19:19:75 in this title");
        let first = cue.files[0].tracks[0].get_index(1).unwrap().begin_time;
        assert_eq!((first.total_seconds(), first.frames()), (19 * 60 + 20, 0));
        let second = &cue.files[0].tracks[1];
        assert_eq!(
            second.get_index(0).unwrap().begin_time.total_seconds(),
            20 * 60
        );
        assert_eq!(
            second.get_index(1).unwrap().begin_time.total_seconds(),
            20 * 60 + 1
        );
    }
}

#[test]
fn unrelated_invalid_timestamps_still_fail_at_the_original_line() {
    for timestamp in ["00:00:76", "00:60:00", "00:60:75", "invalid", "00:00:750"] {
        let contents =
            format!("FILE \"image.flac\" WAVE\n TRACK 01 AUDIO\n  INDEX 01 {timestamp}\n");
        let error = parse(&contents).unwrap_err();
        assert!(error.to_string().contains("line 3"), "{timestamp}: {error}");
    }
}
