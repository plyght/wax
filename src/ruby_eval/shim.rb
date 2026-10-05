require "json"
require "open3"
require "pathname"
require "fileutils"

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
    sequoia: "15", tahoe: "26"
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

class WaxCask
  include WaxConditionals

  ARTIFACTS = %i[
    app pkg binary suite artifact font manpage installer colorpicker dictionary prefpane
    qlplugin screen_saver service input_method keyboard_layout vst_plugin vst3_plugin
    audio_unit_plugin mdimporter bash_completion zsh_completion fish_completion stage_only
  ].freeze

  attr_reader :token

  def initialize(token)
    @token = token
    @names = []
    @artifacts = []
    @depends_on = {}
    @conflicts_with = {}
    @version = nil
    @sha256 = nil
    @url = nil
    @hooks = {}
  end

  attr_reader :hooks

  def version(value = nil)
    return @version if value.nil?
    @version = WaxVersion.new(value == :latest ? "latest" : value.to_s)
  end

  def sha256(value = nil, arm: nil, intel: nil, **_rest)
    value = Hardware::CPU.arm? ? arm : intel if value.nil?
    return @sha256 if value.nil?
    @sha256 = value == :no_check ? "no_check" : value.to_s
  end

  def url(value = nil, **options)
    return @url if value.nil?
    @url = value.to_s
    @url_options = options
  end

  def name(value = nil)
    return @names if value.nil?
    @names << value.to_s
  end

  def desc(value = nil)
    return @desc if value.nil?
    @desc = value.to_s
  end

  def homepage(value = nil)
    return @homepage if value.nil?
    @homepage = value.to_s
  end

  ARTIFACTS.each do |kind|
    define_method(kind) do |*sources, **options|
      values = sources.map(&:to_s)
      values << stringify(options) unless options.empty?
      @artifacts << { kind.to_s => values }
    end
  end

  def uninstall(**options)
    @artifacts << { "uninstall" => [stringify(options)] }
  end

  def zap(**options)
    @artifacts << { "zap" => [stringify(options)] }
  end

  def depends_on(**options)
    @depends_on.merge!(stringify(options))
  end

  def conflicts_with(**options)
    @conflicts_with.merge!(stringify(options))
  end

  %i[preflight postflight uninstall_preflight uninstall_postflight].each do |hook|
    define_method(hook) do |&block|
      next unless block
      @hooks[hook.to_s] = block
      @artifacts << { hook.to_s => block.source_location.join(":") }
    end
  end

  def livecheck(*); end
  def caveats(*); end
  def auto_updates(*); end
  def container(*); end
  def deprecate!(*); end
  def disable!(*); end
  def no_autobump!(*); end

  def language(*_codes, default: false)
    yield if default
  end

  def staged_path; File.join(ENV.fetch("WAX_CASKROOM", "#{HOMEBREW_PREFIX}/Caskroom"), token, version.to_s); end
  def appdir; ENV.fetch("WAX_APPDIR", "/Applications"); end
  def caskroom_path; File.join(ENV.fetch("WAX_CASKROOM", "#{HOMEBREW_PREFIX}/Caskroom"), token); end

  def method_missing(_name, *_args, **_options)
    nil
  end

  def respond_to_missing?(*); true; end

  def to_h
    {
      "token" => token,
      "name" => @names.empty? ? [token] : @names,
      "desc" => @desc,
      "homepage" => @homepage.to_s,
      "version" => @version.to_s,
      "url" => @url.to_s,
      "sha256" => @sha256.to_s,
      "artifacts" => @artifacts,
      "depends_on" => @depends_on,
      "conflicts_with" => @conflicts_with
    }
  end

  private

  def stringify(value)
    case value
    when Hash then value.each_with_object({}) { |(k, v), h| h[k.to_s] = stringify(v) }
    when Array then value.map { |v| stringify(v) }
    when Symbol then value.to_s
    when String, Numeric, true, false, nil then value
    else value.to_s
    end
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

class WaxHook
  def initialize(cask)
    @cask = cask
  end

  def token; @cask.token; end
  def version; @cask.version; end
  def appdir; Pathname.new(@cask.appdir); end
  def staged_path; Pathname.new(ENV["WAX_STAGED_PATH"] || @cask.staged_path); end
  def caskroom_path; Pathname.new(@cask.caskroom_path); end

  def system_command(executable, args: [], sudo: false, must_succeed: false, print_stdout: false,
                     print_stderr: true, input: nil, env: {}, **_rest)
    command = [executable.to_s, *Array(args).map(&:to_s)]
    command = ["/usr/bin/sudo", "-E", "--", *command] if sudo
    environment = env.each_with_object({}) { |(k, v), h| h[k.to_s] = v.nil? ? nil : v.to_s }
    stdout, stderr, status = Open3.capture3(environment, *command, stdin_data: Array(input).join)
    $stderr.print(stdout) if print_stdout
    $stderr.print(stderr) if print_stderr
    if must_succeed && !status.success?
      raise "#{command.join(" ")} exited with #{status.exitstatus}: #{stderr.strip}"
    end
    WaxCommandResult.new(stdout, stderr, status)
  end

  def system_command!(executable, **options)
    system_command(executable, **options, must_succeed: true)
  end

  def set_permissions(paths, permissions)
    Array(paths).each do |path|
      next unless File.exist?(path.to_s)
      system_command!("/bin/chmod", args: ["-R", permissions.to_s, path.to_s],
                                    sudo: !File.writable?(path.to_s))
    end
  end

  def set_ownership(paths, user: ENV.fetch("USER", "root"), group: "staff")
    existing = Array(paths).map(&:to_s).select { |p| File.exist?(p) }
    return if existing.empty?
    system_command!("/usr/sbin/chown", args: ["-R", "#{user}:#{group}", *existing], sudo: true)
  end

  def ohai(*message); $stderr.puts("==> #{message.join(" ")}"); end
  def opoo(*message); $stderr.puts("Warning: #{message.join(" ")}"); end
  def odebug(*); end

  def method_missing(name, *args, **options, &block)
    return @cask.public_send(name, *args, **options, &block) if @cask.respond_to?(name)
    super
  end

  def respond_to_missing?(*); true; end
end

def odie(message)
  raise message.to_s
end

def cask(token, &block)
  cask = WaxCask.new(token.to_s)
  cask.instance_eval(&block)
  if $wax_hook
    hook = cask.hooks[$wax_hook]
    raise "cask #{token} has no #{$wax_hook} block" unless hook
    WaxHook.new(cask).instance_exec(&hook)
    $wax_real_stdout.puts("__WAX_HOOK_OK__")
  else
    $wax_real_stdout.puts("__WAX_JSON__#{JSON.generate(cask.to_h)}")
  end
  $wax_real_stdout.flush
  exit!(0)
end

kind, path, $wax_hook = ARGV
valid = path && (kind == "cask" || (kind == "hook" && $wax_hook))
abort("usage: shim.rb cask <file> | shim.rb hook <file> <name>") unless valid
$wax_hook = nil if kind == "cask"
begin
  load File.expand_path(path)
rescue SystemExit
  raise
rescue Exception => e
  $wax_real_stdout.puts("__WAX_ERROR__#{e.class}: #{e.message}")
  exit!(1)
end
$wax_real_stdout.puts("__WAX_ERROR__no cask definition found in #{path}")
exit!(1)
