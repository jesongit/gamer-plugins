//! Test and packaging fixtures consume the exact same declared schema bytes.
#[cfg(test)]
pub(crate) fn append_schemas<W: std::io::Write + std::io::Seek>(writer: &mut zip::ZipWriter<W>) {
    use std::io::Write;
    let options = zip::write::SimpleFileOptions::default();
    for (name, bytes) in [
        (
            "external_draft_publish_preview.json",
            include_bytes!("../schemas/external_draft_publish_preview.json").as_slice(),
        ),
        (
            "external_draft_publish.json",
            include_bytes!("../schemas/external_draft_publish.json").as_slice(),
        ),
    ] {
        writer
            .start_file(format!("schemas/{name}"), options)
            .unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer
        .start_file("schemas/external_grammar.json", options)
        .unwrap();
    writer
        .write_all(include_bytes!("../schemas/external_grammar.json"))
        .unwrap();
    writer
        .start_file("schemas/external_functions.json", options)
        .unwrap();
    writer
        .write_all(include_bytes!("../schemas/external_functions.json"))
        .unwrap();
    writer
        .start_file("schemas/external_draft_list.json", options)
        .unwrap();
    writer
        .write_all(include_bytes!("../schemas/external_draft_list.json"))
        .unwrap();
    writer
        .start_file("schemas/external_draft_get.json", options)
        .unwrap();
    writer
        .write_all(include_bytes!("../schemas/external_draft_get.json"))
        .unwrap();
    writer
        .start_file("schemas/external_draft_create.json", options)
        .unwrap();
    writer
        .write_all(include_bytes!("../schemas/external_draft_create.json"))
        .unwrap();
    writer
        .start_file("schemas/external_draft_import.json", options)
        .unwrap();
    writer
        .write_all(include_bytes!("../schemas/external_draft_import.json"))
        .unwrap();
    writer
        .start_file("schemas/external_draft_save.json", options)
        .unwrap();
    writer
        .write_all(include_bytes!("../schemas/external_draft_save.json"))
        .unwrap();
    writer
        .start_file("schemas/external_draft_diff.json", options)
        .unwrap();
    writer
        .write_all(include_bytes!("../schemas/external_draft_diff.json"))
        .unwrap();
    writer
        .start_file("schemas/external_draft_validate.json", options)
        .unwrap();
    writer
        .write_all(include_bytes!("../schemas/external_draft_validate.json"))
        .unwrap();
    writer
        .start_file("schemas/external_draft_run.json", options)
        .unwrap();
    writer
        .write_all(include_bytes!("../schemas/external_draft_run.json"))
        .unwrap();
    writer
        .start_file("schemas/external_template_read.json", options)
        .unwrap();
    writer
        .write_all(include_bytes!("../schemas/external_template_read.json"))
        .unwrap();
    writer
        .start_file("schemas/external_draft_crop.json", options)
        .unwrap();
    writer
        .write_all(include_bytes!("../schemas/external_draft_crop.json"))
        .unwrap();
    writer
        .start_file("schemas/external_draft_match.json", options)
        .unwrap();
    writer
        .write_all(include_bytes!("../schemas/external_draft_match.json"))
        .unwrap();
    writer
        .start_file("schemas/external_legacy_list.json", options)
        .unwrap();
    writer
        .write_all(include_bytes!("../schemas/external_legacy_list.json"))
        .unwrap();
    writer
        .start_file("schemas/external_legacy_get.json", options)
        .unwrap();
    writer
        .write_all(include_bytes!("../schemas/external_legacy_get.json"))
        .unwrap();
    writer
        .start_file("schemas/external_legacy_export.json", options)
        .unwrap();
    writer
        .write_all(include_bytes!("../schemas/external_legacy_export.json"))
        .unwrap();
    writer
        .start_file("schemas/external_legacy_import.json", options)
        .unwrap();
    writer
        .write_all(include_bytes!("../schemas/external_legacy_import.json"))
        .unwrap();
    writer
        .start_file("schemas/external_history.json", options)
        .unwrap();
    writer
        .write_all(include_bytes!("../schemas/external_history.json"))
        .unwrap();
}
