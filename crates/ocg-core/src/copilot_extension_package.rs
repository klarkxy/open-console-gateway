//! One immutable, secret-free executable package for every Profile.
use crate::byok_application::{ByokError, ByokResult};
use sha2::{Digest, Sha256};
use std::io::{Cursor, Write};
pub const EXTENSION_ID: &str = "open-console-gateway.copilot";
pub const EXTENSION_VERSION: &str = "0.1.1";
pub const RUNTIME: &[u8] =
    include_bytes!("../../../integrations/copilot-extension/dist/extension.cjs");
const FILES: &[(&str, &[u8])] = &[
    (
        "README.zh-CN.md",
        include_bytes!("../../../integrations/copilot-extension/README.zh-CN.md"),
    ),
    (
        "package.json",
        include_bytes!("../../../integrations/copilot-extension/package.json"),
    ),
    ("dist/extension.cjs", RUNTIME),
    (
        "README.md",
        include_bytes!("../../../integrations/copilot-extension/README.md"),
    ),
    (
        "LICENSE",
        include_bytes!("../../../integrations/copilot-extension/LICENSE"),
    ),
    (
        "THIRD_PARTY_NOTICES",
        include_bytes!("../../../integrations/copilot-extension/THIRD_PARTY_NOTICES"),
    ),
];
pub fn runtime_digest() -> String {
    format!("{:x}", Sha256::digest(RUNTIME))
}
pub fn vsix_bytes() -> ByokResult<Vec<u8>> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);
    let manifest = format!(
        r#"<?xml version="1.0" encoding="utf-8"?><PackageManifest Version="2.0.0" xmlns="http://schemas.microsoft.com/developer/vsx-schema/2011"><Metadata><Identity Language="en-US" Id="copilot" Version="{EXTENSION_VERSION}" Publisher="open-console-gateway"/><DisplayName>Open Console Gateway</DisplayName><Description xml:space="preserve">OCG dynamic model provider</Description><Tags>AI,Chat</Tags><Categories>AI</Categories><GalleryFlags>Public</GalleryFlags><Properties><Property Id="Microsoft.VisualStudio.Code.Engine" Value="^1.141.0"/><Property Id="Microsoft.VisualStudio.Code.ExtensionDependencies" Value=""/><Property Id="Microsoft.VisualStudio.Code.ExtensionPack" Value=""/></Properties><License>extension/LICENSE</License></Metadata><Installation><InstallationTarget Id="Microsoft.VisualStudio.Code"/></Installation><Dependencies/><Assets><Asset Type="Microsoft.VisualStudio.Code.Manifest" Path="extension/package.json" Addressable="true"/></Assets></PackageManifest>"#
    );
    let types = r#"<?xml version="1.0" encoding="utf-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="json" ContentType="application/json"/><Default Extension="cjs" ContentType="application/javascript"/><Default Extension="md" ContentType="text/markdown"/><Default Extension="vsixmanifest" ContentType="text/xml"/><Default Extension="xml" ContentType="text/xml"/><Default Extension="" ContentType="text/plain"/></Types>"#;
    for (name, bytes) in std::iter::once(("extension.vsixmanifest".to_owned(), manifest.as_bytes()))
        .chain(std::iter::once((
            "[Content_Types].xml".to_owned(),
            types.as_bytes(),
        )))
        .chain(FILES.iter().map(|(p, b)| (format!("extension/{p}"), *b)))
    {
        zip.start_file(name, options)
            .map_err(|_| ByokError::internal("Cannot create Copilot package"))?;
        zip.write_all(bytes)
            .map_err(|_| ByokError::internal("Cannot write Copilot package"))?;
    }
    Ok(zip
        .finish()
        .map_err(|_| ByokError::internal("Cannot finalize Copilot package"))?
        .into_inner())
}
#[cfg(test)]
mod tests;
