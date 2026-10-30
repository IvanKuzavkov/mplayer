use super::*;

#[test]
fn format_selection_preserves_precision_before_preferences() {
    use SampleFormat::*;
    let formats = [I16, I24, I32, I64, U8, U16, U32, U64, F32, F64];
    for (source, preferred) in [
        (PcmFormat::Integer(16), F32),
        (PcmFormat::Integer(24), F32),
        (PcmFormat::Integer(32), I32),
        (PcmFormat::Float(32), F32),
        (PcmFormat::Float(64), F64),
    ] {
        let mut ranked = formats;
        ranked.sort_by_key(|&format| source.preference(format));
        assert_eq!(ranked[0], preferred);
        assert!(source.preserved_by(preferred));
        let mut fallback = [I16, F32, I24];
        fallback.sort_by_key(|&format| source.preference(format));
        assert_eq!(fallback[0], F32);
    }
    assert!(!PcmFormat::Integer(32).preserved_by(I24));
    assert!(!PcmFormat::Integer(32).preserved_by(F32));
    assert!(PcmFormat::Integer(24).preserved_by(I24));
    assert!(PcmFormat::Integer(32).preserved_by(U32));
    assert!(!PcmFormat::Float(32).preserved_by(I64));
    assert!(!PcmFormat::Float(64).preserved_by(F32));
}
