use super::*;
use crate::api::CaskArtifact;

async fn eval(source: &str) -> Option<Result<CaskDetails>> {
    let ruby = find_ruby()?;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("fixture.rb");
    std::fs::write(&path, source).unwrap();
    Some(eval_cask(&ruby, &path).await)
}

#[tokio::test]
async fn evaluates_arch_version_helpers_and_artifacts() {
    let source = r##"
cask "fixture" do
  arch arm: "arm64", intel: "x64"
  version "1.2.3,456"
  sha256 arm:   "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
         intel: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
  url "https://example.invalid/#{version.before_comma}/Fixture-#{version.csv.second || version.after_comma}-#{arch}.dmg",
      verified: "example.invalid/"
  name "Fixture"
  desc "Fixture cask"
  homepage "https://example.invalid/"
  depends_on macos: ">= :big_sur"
  on_sonoma :or_newer do
    binary "#{appdir}/Fixture.app/Contents/MacOS/fixture", target: "fixture"
  end
  app "Fixture.app"
  postflight do
    system_command "/usr/bin/true"
  end
  zap trash: "~/Library/Fixture"
end
"##;
    let Some(details) = eval(source).await else {
        return;
    };
    let details = details.unwrap();
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    };
    assert_eq!(details.version, "1.2.3,456");
    assert_eq!(
        details.url,
        format!("https://example.invalid/1.2.3/Fixture-456-{arch}.dmg")
    );
    assert_eq!(details.name, vec!["Fixture".to_string()]);
    let artifacts = details.artifacts.unwrap();
    assert!(artifacts
        .iter()
        .any(|a| matches!(a, CaskArtifact::App { app } if app[0] == "Fixture.app")));
    assert!(artifacts
        .iter()
        .any(|a| matches!(a, CaskArtifact::Postflight { .. })));
}

#[tokio::test]
async fn runs_arbitrary_ruby_and_ignores_cask_stdout() {
    let source = r##"
require "digest"
puts "noise on stdout"
cask "dynamic" do
  computed = Digest::SHA256.hexdigest("wax")
  version :latest
  sha256 :no_check
  url "https://example.invalid/#{computed[0, 8]}.zip"
  app "Dynamic.app"
end
"##;
    let Some(details) = eval(source).await else {
        return;
    };
    let details = details.unwrap();
    assert_eq!(details.version, "latest");
    assert_eq!(details.sha256, "no_check");
    assert_eq!(details.url, "https://example.invalid/57abfafa.zip");
}

#[tokio::test]
async fn reports_ruby_errors() {
    let source = r##"
cask "broken" do
  odie "Unsupported platform"
end
"##;
    let Some(result) = eval(source).await else {
        return;
    };
    let err = result.unwrap_err().to_string();
    assert!(err.contains("Unsupported platform"), "{err}");
}

#[test]
fn marker_line_is_found_after_noise() {
    assert_eq!(
        parse_output("noise\n__WAX_JSON__{\"a\":1}\n"),
        Some("{\"a\":1}")
    );
    assert_eq!(parse_output("nothing here"), None);
}
