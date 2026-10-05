require "json"
require "open3"
require "pathname"
require "fileutils"
require "tmpdir"
require "digest"

$stdout.sync = true
$wax_real_stdout = $stdout.dup
$stdout = $stderr

module WaxEnv
  def self.mac?
    ENV["WAX_OS"] == "macos"
  end

  def self.arm?
    ENV["WAX_ARCH"] == "arm64"
  end
end

class Array
  def second; self[1]; end unless method_defined?(:second)
  def third; self[2]; end unless method_defined?(:third)
  def fourth; self[3]; end unless method_defined?(:fourth)
  def fifth; self[4]; end unless method_defined?(:fifth)
end

class Object
  def blank?; respond_to?(:empty?) ? empty? : !self; end unless method_defined?(:blank?)
  def present?; !blank?; end unless method_defined?(:present?)
end

HOMEBREW_PREFIX = ENV.fetch("HOMEBREW_PREFIX", "/opt/homebrew") unless defined?(HOMEBREW_PREFIX)

module Hardware
  module CPU
    def self.arm?; WaxEnv.arm?; end
    def self.intel?; !WaxEnv.arm?; end
    def self.in_rosetta2?; false; end
    def self.is_64_bit?; true; end
    def self.is_32_bit?; false; end
    def self.avx2?; !WaxEnv.arm?; end
    def self.type; WaxEnv.arm? ? :arm : :intel; end
    def self.arch; WaxEnv.arm? ? :arm64 : :x86_64; end
    def self.method_missing(*); false; end
    def self.respond_to_missing?(*); true; end
  end
end

module OS
  def self.mac?; WaxEnv.mac?; end
  def self.linux?; !WaxEnv.mac?; end
  def self.kernel_name; WaxEnv.mac? ? "Darwin" : "Linux"; end
end

class WaxMacOSVersion
  include Comparable
  SYMBOLS = {
    el_capitan: "10.11", sierra: "10.12", high_sierra: "10.13", mojave: "10.14",
    catalina: "10.15", big_sur: "11", monterey: "12", ventura: "13", sonoma: "14",
    sequoia: "15", tahoe: "26", golden_gate: "27"
  }.freeze

  def self.from(value)
    return value if value.is_a?(WaxMacOSVersion)
    raw = value.is_a?(Symbol) ? SYMBOLS.fetch(value) { "99" } : value.to_s
    new(raw)
  end

  attr_reader :parts

  def initialize(raw)
    @raw = raw.to_s
    @parts = @raw.split(".").map(&:to_i)
  end

  def <=>(other)
    other = WaxMacOSVersion.from(other)
    width = [parts.length, other.parts.length].max
    (parts + [0] * (width - parts.length)) <=> (other.parts + [0] * (width - other.parts.length))
  end

  def to_s; @raw; end
  def to_sym; SYMBOLS.key(@raw) || :unknown; end
  def major; parts[0]; end
end

module MacOS
  def self.version
    WaxMacOSVersion.new(ENV.fetch("WAX_MACOS_VERSION", "26"))
  end

  def self.method_missing(*); nil; end
  def self.respond_to_missing?(*); true; end
end

class WaxVersion < String
  def latest?; self == "latest"; end

  def major; WaxVersion.new(dot_parts[0].to_s); end
  def minor; WaxVersion.new(dot_parts[1].to_s); end
  def patch; WaxVersion.new(dot_parts[2].to_s); end
  def major_minor; WaxVersion.new(dot_parts[0, 2].join(".")); end
  def major_minor_patch; WaxVersion.new(dot_parts[0, 3].join(".")); end
  def minor_patch; WaxVersion.new(dot_parts[1, 2].join(".")); end
  def csv; split(",").map { |p| WaxVersion.new(p) }; end
  def before_comma; WaxVersion.new(split(",", 2)[0].to_s); end
  def after_comma; WaxVersion.new(split(",", 2)[1].to_s); end
  def before_colon; WaxVersion.new(split(":", 2)[0].to_s); end
  def after_colon; WaxVersion.new(split(":", 2)[1].to_s); end
  def no_dots; WaxVersion.new(delete(".")); end
  def no_hyphens; WaxVersion.new(delete("-")); end
  def no_dividers; WaxVersion.new(delete(".-_,:")); end
  def dots_to_underscores; WaxVersion.new(tr(".", "_")); end
  def dots_to_hyphens; WaxVersion.new(tr(".", "-")); end
  def hyphens_to_dots; WaxVersion.new(tr("-", ".")); end
  def underscores_to_dots; WaxVersion.new(tr("_", ".")); end
  def chomp(*args); WaxVersion.new(super); end

  private

  def dot_parts
    before_comma_raw = split(",", 2)[0].to_s
    before_comma_raw.split(/[._-]/)
  end
end

module WaxConditionals
  def on_macos; yield if OS.mac?; end
  def on_linux; yield if OS.linux?; end
  def on_arm; yield if Hardware::CPU.arm?; end
  def on_intel; yield if Hardware::CPU.intel?; end

  WaxMacOSVersion::SYMBOLS.each_key do |codename|
    define_method("on_#{codename}") do |bound = nil, &block|
      next unless OS.mac?
      current = MacOS.version
      matched = case bound
                when :or_newer then current >= codename
                when :or_older then current <= codename
                else current.major == WaxMacOSVersion.from(codename).major
                end
      block.call if matched
    end
  end

  def on_system(*conditions, macos: nil)
    yield if conditions.empty? || conditions.any? { |c| c == :linux ? OS.linux? : OS.mac? }
  end

  def arch(arm: nil, intel: nil)
    return @wax_arch if arm.nil? && intel.nil?
    @wax_arch = Hardware::CPU.arm? ? arm : intel
  end

  def os(macos: nil, linux: nil)
    return @wax_os if macos.nil? && linux.nil?
    @wax_os = OS.mac? ? macos : linux
  end
end

class WaxCommandResult
  attr_reader :stdout, :stderr, :exit_status

  def initialize(stdout, stderr, status)
    @stdout = stdout
    @stderr = stderr
    @exit_status = status.exitstatus
    @success = status.success?
  end

  def success?; @success; end
  def merged_output; stdout + stderr; end
  def to_s; stdout; end
end

def odie(message)
  raise message.to_s
end
