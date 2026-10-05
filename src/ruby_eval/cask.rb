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
