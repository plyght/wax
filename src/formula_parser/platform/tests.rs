use super::{select_url_sha, Target};

const MAC_ARM: Target = Target {
    mac: true,
    arm: true,
};
const MAC_INTEL: Target = Target {
    mac: true,
    arm: false,
};
const LINUX_ARM: Target = Target {
    mac: false,
    arm: true,
};
const LINUX_INTEL: Target = Target {
    mac: false,
    arm: false,
};

fn sha(c: char) -> String {
    c.to_string().repeat(64)
}

fn url_for(formula: &str, target: Target) -> Option<String> {
    select_url_sha(formula, target).map(|(url, _)| url)
}

#[test]
fn if_elsif_else_chain_with_version_interpolation() {
    let formula = format!(
        r#"class Bun < Formula
  version "1.4.2"
  livecheck do
    url "https://github.com/oven-sh/bun/releases/latest"
  end
  if OS.mac?
    if Hardware::CPU.arm? || Hardware::CPU.in_rosetta2?
      url "https://example.com/bun-v#{{version}}/bun-darwin-aarch64.zip"
      sha256 "{a}" # bun-darwin-aarch64.zip
    elsif Hardware::CPU.avx2?
      url "https://example.com/bun-v#{{version}}/bun-darwin-x64.zip"
      sha256 "{b}"
    else
      url "https://example.com/bun-v#{{version}}/bun-darwin-x64-baseline.zip"
      sha256 "{c}"
    end
  elsif OS.linux?
    if Hardware::CPU.arm?
      url "https://example.com/bun-v#{{version}}/bun-linux-aarch64.zip"
      sha256 "{d}"
    else
      url "https://example.com/bun-v#{{version}}/bun-linux-x64.zip"
      sha256 "{e}"
    end
  else
    odie "Unsupported platform"
  end
  def install
    bin.install "bun"
  end
end
"#,
        a = sha('a'),
        b = sha('b'),
        c = sha('c'),
        d = sha('d'),
        e = sha('e')
    );
    assert_eq!(
        select_url_sha(&formula, MAC_ARM),
        Some((
            "https://example.com/bun-v1.4.2/bun-darwin-aarch64.zip".into(),
            sha('a')
        ))
    );
    assert_eq!(
        url_for(&formula, MAC_INTEL).unwrap(),
        "https://example.com/bun-v1.4.2/bun-darwin-x64.zip"
    );
    assert_eq!(
        url_for(&formula, LINUX_ARM).unwrap(),
        "https://example.com/bun-v1.4.2/bun-linux-aarch64.zip"
    );
    assert_eq!(
        url_for(&formula, LINUX_INTEL).unwrap(),
        "https://example.com/bun-v1.4.2/bun-linux-x64.zip"
    );
}

#[test]
fn compound_top_level_conditions_and_heredocs() {
    let formula = format!(
        r#"class Terraform < Formula
  version "1.16.4"
  if OS.mac? && Hardware::CPU.intel?
    url "https://example.com/terraform_darwin_amd64.zip"
    sha256 "{a}"
  end
  if OS.mac? && Hardware::CPU.arm?
    url "https://example.com/terraform_darwin_arm64.zip"
    sha256 "{b}"
  end
  if OS.linux? && Hardware::CPU.arm? && !Hardware::CPU.is_64_bit?
    url "https://example.com/terraform_linux_arm.zip"
    sha256 "{c}"
  end
  if OS.linux? && Hardware::CPU.arm? && Hardware::CPU.is_64_bit?
    url "https://example.com/terraform_linux_arm64.zip"
    sha256 "{d}"
  end
  def install
    bin.install "terraform"
    (fish_completion/"terraform.fish").write <<~EOS
      function __complete_terraform
          terraform
      end
    EOS
  end
end
"#,
        a = sha('a'),
        b = sha('b'),
        c = sha('c'),
        d = sha('d')
    );
    assert_eq!(
        url_for(&formula, MAC_ARM).unwrap(),
        "https://example.com/terraform_darwin_arm64.zip"
    );
    assert_eq!(
        url_for(&formula, MAC_INTEL).unwrap(),
        "https://example.com/terraform_darwin_amd64.zip"
    );
    assert_eq!(
        url_for(&formula, LINUX_ARM).unwrap(),
        "https://example.com/terraform_linux_arm64.zip"
    );
    assert_eq!(url_for(&formula, LINUX_INTEL), None);
}

#[test]
fn goreleaser_blocks_with_nested_install_methods() {
    let formula = format!(
        r#"class Opencode < Formula
  version "1.18.34"
  depends_on "ripgrep"
  on_macos do
    if Hardware::CPU.intel?
      url "https://example.com/opencode-darwin-x64.zip"
      sha256 "{a}"

      def install
        bin.install "opencode"
      end
    end
    if Hardware::CPU.arm?
      url "https://example.com/opencode-darwin-arm64.zip"
      sha256 "{b}"

      def install
        bin.install "opencode"
      end
    end
  end
  on_linux do
    if Hardware::CPU.intel? and Hardware::CPU.is_64_bit?
      url "https://example.com/opencode-linux-x64.tar.gz"
      sha256 "{c}"
      def install
        bin.install "opencode"
      end
    end
  end
end
"#,
        a = sha('a'),
        b = sha('b'),
        c = sha('c')
    );
    assert_eq!(
        select_url_sha(&formula, MAC_ARM),
        Some((
            "https://example.com/opencode-darwin-arm64.zip".into(),
            sha('b')
        ))
    );
    assert_eq!(
        url_for(&formula, MAC_INTEL).unwrap(),
        "https://example.com/opencode-darwin-x64.zip"
    );
    assert_eq!(
        url_for(&formula, LINUX_INTEL).unwrap(),
        "https://example.com/opencode-linux-x64.tar.gz"
    );
    assert_eq!(url_for(&formula, LINUX_ARM), None);
}

#[test]
fn ignores_resources_bottles_and_unknown_conditions() {
    let formula = format!(
        r#"class Tool < Formula
  url "https://example.com/tool-1.0.tar.gz"
  sha256 "{a}"
  bottle do
    sha256 cellar: :any, arm64_sonoma: "{b}"
  end
  resource "extra" do
    url "https://example.com/extra.tar.gz"
    sha256 "{c}"
  end
  if MacOS.version >= :sonoma
    url "https://example.com/never.tar.gz"
    sha256 "{d}"
  else
    url "https://example.com/never-else.tar.gz"
    sha256 "{d}"
  end
end
"#,
        a = sha('a'),
        b = sha('b'),
        c = sha('c'),
        d = sha('d')
    );
    for target in [MAC_ARM, LINUX_INTEL] {
        assert_eq!(select_url_sha(&formula, target), None);
    }
}

#[test]
fn requires_a_checksum_and_resolved_interpolation() {
    let unresolved = r#"class T < Formula
  on_macos do
    url "https://example.com/#{name}.zip"
    sha256 "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
  end
end
"#;
    assert_eq!(select_url_sha(unresolved, MAC_ARM), None);
    let no_sha = r#"class T < Formula
  on_macos do
    url "https://example.com/t.zip"
  end
end
"#;
    assert_eq!(select_url_sha(no_sha, MAC_ARM), None);
}
