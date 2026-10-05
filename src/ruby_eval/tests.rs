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

#[tokio::test]
async fn runs_postflight_with_homebrew_helpers() {
    let Some(ruby) = find_ruby() else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let staged = tmp.path().join("staged");
    std::fs::create_dir_all(&staged).unwrap();
    std::fs::write(staged.join("tool"), "#!/bin/sh\n").unwrap();
    let rb = tmp.path().join("hooked.rb");
    std::fs::write(
        &rb,
        r##"
cask "hooked" do
  version "1.0"
  sha256 :no_check
  url "https://example.invalid/hooked.zip"
  app "Hooked.app"
  postflight do
    result = system_command "/bin/echo", args: ["hello", token]
    File.write(staged_path.join("marker"), "#{result.stdout.strip} #{version}")
    set_permissions staged_path.join("tool"), "0755"
    ohai "postflight done"
  end
  preflight do
    system_command "/usr/bin/false", must_succeed: true
  end
end
"##,
    )
    .unwrap();

    let log = run_hook(&ruby, &rb, "postflight", &staged).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(staged.join("marker")).unwrap(),
        "hello hooked 1.0"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(staged.join("tool"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
    }
    assert!(log.contains("postflight done"));

    let err = run_hook(&ruby, &rb, "preflight", &staged)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("preflight failed"), "{err}");
    assert!(run_hook(&ruby, &rb, "uninstall_postflight", &staged)
        .await
        .is_err());
}

const FORMULA: &str = r##"
class Fixture < Formula
  desc "Fixture formula"
  homepage "https://example.invalid/"
  version "2.1.0"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://example.invalid/fixture-#{version}-darwin-arm64.tar.gz"
      sha256 "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    else
      url "https://example.invalid/fixture-#{version}-darwin-x64.tar.gz"
      sha256 "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    end
  end
  on_linux do
    url "https://example.invalid/fixture-#{version}-linux.tar.gz"
    sha256 "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
  end

  head do
    url "https://example.invalid/fixture.git", branch: "main"
  end

  depends_on "go" => :build
  depends_on "ripgrep"

  def install
    inreplace "fixture.sh", "@VERSION@", version.to_s
    libexec.install "fixture.sh", "data"
    bin.write_exec_script libexec/"fixture.sh"
    bin.install_symlink libexec/"fixture.sh" => "fx"
    (share/"doc").install "README" if File.exist?("README")
    generate_completions_from_executable(libexec/"fixture.sh", "completion")
    (etc/"never").mkpath if build.with?("never")
  end
end
"##;

#[tokio::test]
async fn formula_meta_resolves_platform_url_and_ignores_head() {
    let Some(ruby) = find_ruby() else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let rb = tmp.path().join("fixture.rb");
    std::fs::write(&rb, FORMULA).unwrap();
    let meta = eval_formula_meta(&ruby, &rb).await.unwrap();
    let expected = match (cfg!(target_os = "macos"), cfg!(target_arch = "aarch64")) {
        (true, true) => "https://example.invalid/fixture-2.1.0-darwin-arm64.tar.gz",
        (true, false) => "https://example.invalid/fixture-2.1.0-darwin-x64.tar.gz",
        _ => "https://example.invalid/fixture-2.1.0-linux.tar.gz",
    };
    assert_eq!(meta.url.as_deref(), Some(expected));
    assert_eq!(meta.version.as_deref(), Some("2.1.0"));
    assert!(!meta.patches);
    assert!(!meta.keg_only);
}

#[tokio::test]
async fn formula_install_runs_homebrew_install_api() {
    let Some(ruby) = find_ruby() else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let rb = tmp.path().join("fixture.rb");
    std::fs::write(&rb, FORMULA).unwrap();
    let build = tmp.path().join("build");
    std::fs::create_dir_all(build.join("data")).unwrap();
    std::fs::write(
        build.join("fixture.sh"),
        "#!/bin/sh\nif [ \"$1\" = completion ]; then echo \"complete $2\"; else echo @VERSION@; fi\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            build.join("fixture.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    std::fs::write(build.join("README"), "readme").unwrap();
    let keg = tmp.path().join("Cellar/fixture/2.1.0");
    std::fs::create_dir_all(&keg).unwrap();

    run_formula_install(
        &ruby,
        &rb,
        FormulaInstall {
            name: "fixture",
            version: "2.1.0",
            buildpath: &build,
            prefix: &keg,
            path_prefix: tmp.path(),
        },
    )
    .await
    .unwrap();

    let script = std::fs::read_to_string(keg.join("libexec/fixture.sh")).unwrap();
    assert!(script.contains("echo 2.1.0"));
    assert!(std::fs::read_to_string(keg.join("bin/fixture.sh"))
        .unwrap()
        .contains(&keg.join("libexec/fixture.sh").display().to_string()));
    assert_eq!(
        std::fs::read_link(keg.join("bin/fx")).unwrap(),
        std::path::Path::new("../libexec/fixture.sh")
    );
    assert!(keg.join("libexec/data").is_dir());
    assert!(keg.join("share/doc/README").exists());
    assert_eq!(
        std::fs::read_to_string(keg.join("share/zsh/site-functions/_fixture")).unwrap(),
        "complete zsh\n"
    );
}

#[tokio::test]
async fn formula_install_reports_unsupported_api_and_patches() {
    let Some(ruby) = find_ruby() else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let rb = tmp.path().join("odd.rb");
    std::fs::write(
        &rb,
        r##"
class Odd < Formula
  url "https://example.invalid/odd-1.0.tar.gz"
  sha256 "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
  patch do
    url "https://example.invalid/fix.patch"
  end
  def install
    some_future_helper "x"
  end
end
"##,
    )
    .unwrap();
    assert!(eval_formula_meta(&ruby, &rb).await.unwrap().patches);
    let keg = tmp.path().join("keg");
    std::fs::create_dir_all(&keg).unwrap();
    let err = run_formula_install(
        &ruby,
        &rb,
        FormulaInstall {
            name: "odd",
            version: "1.0",
            buildpath: tmp.path(),
            prefix: &keg,
            path_prefix: tmp.path(),
        },
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("some_future_helper"), "{err}");
}
