//! Reproduction: `uri_matches_template` panics on multi-byte path segments when
//! the matcher must backtrack off a char boundary (router resource routing).

use conduit_lib::router::uri_matches_template;

#[test]
fn uri_matches_template_multibyte_backtrack_does_not_panic() {
    // "café" is 5 UTF-8 bytes (é is two bytes). The Level-1 matcher walks the
    // variable segment with raw byte indices; when the trailing literal forces
    // backtrack, a non-boundary index slices the string and panics.
    //
    // Expected: a bool (true if the URI is an expansion of the template).
    // Actual on origin/main: panic "byte index is not a char boundary".
    let uri = "file://café";
    let template = "file://{name}é";
    let matched = uri_matches_template(uri, template);
    assert!(
        matched,
        "file://café should match Level-1 template file://{{name}}é"
    );
}

#[test]
fn ascii_template_matching_is_unchanged() {
    // Guard the existing Level-1 and {+var} results while the backtrack walk changes
    // from byte offsets to char offsets.
    assert!(uri_matches_template(
        "fixture://item/06",
        "fixture://item/{id}"
    ));
    assert!(!uri_matches_template(
        "fixture://item/06/extra",
        "fixture://item/{id}"
    ));
    assert!(uri_matches_template("file:///a/b/c.txt", "file:///{+path}"));
}

#[test]
fn multibyte_reserved_expansion_also_survives() {
    // `{+var}` uses the `.+` branch, which had the same raw-byte backtrack.
    assert!(uri_matches_template(
        "file:///naïve/päth.txt",
        "file:///{+path}"
    ));
    assert!(uri_matches_template("file:///日本語/x", "file:///{+path}"));
}
