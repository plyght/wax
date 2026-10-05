class Pathname
  alias wax_original_write write

  def write(content, *args, **options)
    dirname.mkpath
    options.empty? ? wax_original_write(content, *args) : wax_original_write(content, *args, **options)
  end

  def append_lines(content, **_options)
    dirname.mkpath
    File.open(to_s, "a") { |f| f.puts(content) }
  end

  def install(*sources)
    mkpath
    sources.flatten.each do |source|
      if source.is_a?(Hash)
        source.each { |from, to| wax_install_one(Pathname.new(from.to_s), self / to.to_s) }
      else
        from = Pathname.new(source.to_s)
        wax_install_one(from, self / from.basename)
      end
    end
  end

  def install_symlink(*sources)
    mkpath
    sources.flatten.each do |source|
      if source.is_a?(Hash)
        source.each { |from, to| wax_symlink(Pathname.new(from.to_s), self / to.to_s) }
      else
        from = Pathname.new(source.to_s)
        wax_symlink(from, self / from.basename)
      end
    end
  end

  def write_exec_script(*targets)
    mkpath
    targets.flatten.each do |target|
      target = Pathname.new(target.to_s)
      script = self / target.basename
      script.write("#!/bin/bash\nexec \"#{target}\" \"$@\"\n")
      script.chmod(0o755)
    end
  end

  def write_env_script(target, args_or_env, env = nil)
    args = env.nil? ? "" : " #{args_or_env}"
    env = args_or_env if env.nil?
    exports = env.map { |key, value| "#{key}=\"#{value}\"" }.join(" ")
    dirname.mkpath
    write("#!/bin/bash\n#{exports} exec \"#{target}\"#{args} \"$@\"\n")
    chmod(0o755)
  end

  def write_jar_script(jar, name, java_opts = "", java_version: nil)
    mkpath
    script = self / name.to_s
    script.write("#!/bin/bash\nexec java #{java_opts} -jar \"#{jar}\" \"$@\"\n")
    script.chmod(0o755)
  end

  def atomic_write(content)
    dirname.mkpath
    write(content)
  end

  private

  def wax_install_one(from, to)
    raise Errno::ENOENT, from.to_s unless from.exist? || from.symlink?
    to.dirname.mkpath
    FileUtils.rm_rf(to.to_s) if to.exist? || to.symlink?
    FileUtils.mv(from.to_s, to.to_s)
  end

  def wax_symlink(from, to)
    to.dirname.mkpath
    FileUtils.rm_f(to.to_s)
    target = from.absolute? ? from.relative_path_from(to.dirname) : from
    File.symlink(target.to_s, to.to_s)
  end
end

class WaxInreplaceString < String
  def change_make_var!(flag, new_value)
    sub!(/^#{Regexp.escape(flag)}[ \t]*[\\?\+\:\!]?=[ \t]*(.*)$/, "#{flag}=#{new_value}")
  end

  def remove_make_var!(flags)
    Array(flags).each { |flag| sub!(/^#{Regexp.escape(flag)}[ \t]*[\\?\+\:\!]?=(.*)$\n/, "") }
  end

  def get_make_var(flag)
    self[/^#{Regexp.escape(flag)}[ \t]*[\\?\+\:\!]?=[ \t]*(.*)$/, 1]
  end
end

module ENVExtensions
  def deparallelize; self["MAKEFLAGS"] = "-j1"; end
  def parallelize; end
  def append(key, value, separator = " ")
    self[key] = [self[key], value].compact.reject(&:empty?).join(separator)
  end
  def prepend(key, value, separator = " ")
    self[key] = [value, self[key]].compact.reject(&:empty?).join(separator)
  end
  def append_path(key, path); append(key, path.to_s, File::PATH_SEPARATOR); end
  def prepend_path(key, path); prepend(key, path.to_s, File::PATH_SEPARATOR); end
  def prepend_create_path(key, path)
    FileUtils.mkdir_p(path.to_s)
    prepend_path(key, path)
  end
  def method_missing(name, *args)
    return nil if name.to_s.end_with?("!") || %i[cxx11 libcxx O0 O1 O3 permit_arch_flags
                                               runtime_cpu_detection universal_binary
                                               append_to_cflags append_to_rustflags
                                               refurbish_args].include?(name)
    super
  end
  def respond_to_missing?(*); true; end
  def cc; self["CC"] || "cc"; end
  def cxx; self["CXX"] || "c++"; end
end
ENV.extend(ENVExtensions)

class WaxResource
  include WaxConditionals

  def initialize(name)
    @name = name
  end

  def url(value = nil, **_options)
    return @url if value.nil?
    @url = value.to_s
  end

  def sha256(value = nil)
    return @sha256 if value.nil?
    @sha256 = value.to_s
  end

  def version(value = nil)
    return @version if value.nil?
    @version = WaxVersion.new(value.to_s)
  end

  def method_missing(*); nil; end
  def respond_to_missing?(*); true; end

  def stage(target = nil, &block)
    dir = Pathname.new(Dir.mktmpdir("wax-resource-"))
    archive = dir / File.basename(@url.split("?").first)
    ok = Kernel.system("curl", "-fsSL", "--retry", "3", "-o", archive.to_s, @url)
    raise "could not download resource #{@name}" unless ok
    if @sha256 && @sha256 != "no_check"
      actual = Digest::SHA256.file(archive.to_s).hexdigest
      raise "resource #{@name} checksum mismatch: #{actual}" unless actual == @sha256
    end
    unpacked = dir / "unpacked"
    unpacked.mkpath
    WaxArchive.extract(archive, unpacked)
    root = WaxArchive.single_root(unpacked)
    if target
      Pathname.new(target.to_s).mkpath
      root.children.each { |child| FileUtils.mv(child.to_s, Pathname.new(target.to_s).to_s) }
      return Pathname.new(target.to_s)
    end
    block ? Dir.chdir(root.to_s) { block.call(root) } : root
  end
end

module WaxArchive
  def self.extract(archive, into)
    name = archive.basename.to_s.downcase
    ok = if name.end_with?(".zip")
           Kernel.system("unzip", "-qo", archive.to_s, "-d", into.to_s)
         elsif name =~ /\.(tar(\.(gz|bz2|xz|zst))?|tgz|tbz2?|txz)$/
           Kernel.system("tar", "-xf", archive.to_s, "-C", into.to_s)
         else
           FileUtils.cp(archive.to_s, into.to_s)
         end
    raise "could not extract #{archive}" unless ok
  end

  def self.single_root(dir)
    entries = dir.children.reject { |c| c.basename.to_s.start_with?(".") }
    entries.length == 1 && entries.first.directory? ? entries.first : dir
  end
end

module Utils
  def self.safe_popen_read(*command, **_options)
    output, status = Open3.capture2(*command.flatten.map(&:to_s))
    raise "Failed executing: #{command.join(" ")}" unless status.success?
    output
  end

  def self.popen_read(*command, **_options)
    Open3.capture2(*command.flatten.map(&:to_s)).first
  end

  def self.safe_popen_write(*command, **_options)
    IO.popen(command.flatten.map(&:to_s), "w") { |io| yield io }
    raise "Failed executing: #{command.join(" ")}" unless $?.success?
  end
end

class WaxPatch
  attr_accessor :data

  def initialize(strip)
    @strip = strip.to_s
  end

  def url(value = nil, **_options)
    return @url if value.nil?
    @url = value.to_s
  end

  def sha256(value = nil)
    return @sha256 if value.nil?
    @sha256 = value.to_s
  end

  def apply(*files)
    @files = files.flatten.map(&:to_s)
  end

  def directory(value)
    @directory = value.to_s
  end

  def method_missing(*); nil; end
  def respond_to_missing?(*); true; end

  def apply!(buildpath)
    patches = patch_files
    Dir.chdir(@directory ? File.join(buildpath.to_s, @directory) : buildpath.to_s) do
      patches.each do |file|
        ok = Kernel.system("patch", "-g", "0", "-f", "-#{@strip}", "-i", file)
        raise "failed to apply patch #{@url || "inline"}" unless ok
      end
    end
  end

  private

  def patch_files
    dir = Dir.mktmpdir("wax-patch-")
    if data
      path = File.join(dir, "inline.patch")
      File.write(path, data)
      return [path]
    end
    raise "patch without url" unless @url
    file = File.join(dir, File.basename(@url.split("?").first))
    raise "could not download patch #{@url}" unless Kernel.system("curl", "-fsSL", "--retry", "3", "-o", file, @url)
    if @sha256 && Digest::SHA256.file(file).hexdigest != @sha256
      raise "patch #{@url} checksum mismatch"
    end
    return [file] unless @files
    unpacked = File.join(dir, "unpacked")
    FileUtils.mkdir_p(unpacked)
    WaxArchive.extract(Pathname.new(file), Pathname.new(unpacked))
    root = WaxArchive.single_root(Pathname.new(unpacked))
    @files.map { |f| (root / f).to_s }
  end
end

class WaxBuildOptions
  def with?(*); false; end
  def without?(*); true; end
  def head?; false; end
  def stable?; true; end
  def bottle?; false; end
  def method_missing(*); false; end
  def respond_to_missing?(*); true; end
end

class Formula
  include FileUtils
  extend WaxConditionals

  class << self
    def inherited(subclass)
      super
      $wax_formula_class = subclass
    end

    def wax
      @wax ||= { dependencies: [], build_dependencies: [], resources: {} }
    end

    %i[desc homepage license revision].each do |field|
      define_method(field) do |value = nil, **_options|
        return wax[field] if value.nil?
        wax[field] = value
      end
    end

    def url(value = nil, **options)
      return wax[:url] if value.nil?
      return if @wax_in_head
      wax[:url] = value.to_s
      wax[:using] = options[:using].to_s if options[:using]
    end

    def mirror(*); end

    def sha256(value = nil, **_tags)
      return wax[:sha256] if value.nil?
      return if @wax_in_head
      wax[:sha256] = value.to_s
    end

    def version(value = nil)
      return wax[:version] if value.nil?
      return if @wax_in_head
      wax[:version] = value.to_s
    end

    def head(value = nil, **_options, &block)
      wax[:head] = value.to_s if value
      return unless block
      @wax_in_head = true
      instance_eval(&block)
    ensure
      @wax_in_head = false
    end

    def stable(&block)
      instance_eval(&block)
    end

    def depends_on(spec)
      if spec.is_a?(Hash)
        spec.each do |name, tags|
          next unless name.is_a?(String)
          tags = Array(tags)
          next if tags.include?(:test)
          (tags.include?(:build) ? wax[:build_dependencies] : wax[:dependencies]) << name
        end
      elsif spec.is_a?(String)
        wax[:dependencies] << spec
      end
    end

    def uses_from_macos(spec, **_options)
      depends_on(spec) if OS.linux?
    end

    def resource(name, &block)
      res = WaxResource.new(name)
      res.instance_eval(&block) if block
      wax[:resources][name] = res
    end

    def resources
      wax[:resources]
    end

    def keg_only(*); wax[:keg_only] = true; end

    def build; WaxBuildOptions.new; end

    def patch(strip = :p1, source = nil, &block)
      return if @wax_in_head
      strip, source = :p1, strip unless strip.is_a?(Symbol) && strip.to_s.match?(/\Ap\d\z/)
      spec = WaxPatch.new(strip)
      if block
        spec.instance_eval(&block)
      elsif source == :DATA
        spec.data = File.read($wax_formula_path).split(/^__END__$/, 2)[1].to_s.sub(/\A\n/, "")
      elsif source.is_a?(String)
        spec.data = source
      else
        wax[:unsupported_patch] = true
        return
      end
      (wax[:patches] ||= []) << spec
    end

    %i[bottle livecheck test service fails_with option conflicts_with deprecate! disable!
       pour_bottle? link_overwrite skip_clean cxxstdlib_check compatibility_version
       no_autobump! deny_network_access! allow_network_access! preserve_rpath env
       uses_from_macos_since].each do |dsl|
      define_method(dsl) { |*_args, **_options, &_block| nil }
    end

    def method_missing(*); nil; end
    def respond_to_missing?(*); true; end
  end

  def name; ENV.fetch("WAX_FORMULA_NAME"); end
  def full_name; name; end
  def version; WaxVersion.new(ENV.fetch("WAX_FORMULA_VERSION", self.class.version.to_s)); end
  def revision; self.class.revision.to_i; end
  def build; WaxBuildOptions.new; end
  def head?; false; end
  def stable?; true; end

  def prefix; Pathname.new(ENV.fetch("WAX_PREFIX")); end
  def buildpath; Pathname.new(ENV.fetch("WAX_BUILDPATH")); end
  def bin; prefix / "bin"; end
  def sbin; prefix / "sbin"; end
  def lib; prefix / "lib"; end
  def libexec; prefix / "libexec"; end
  def include; prefix / "include"; end
  def share; prefix / "share"; end
  def pkgshare; share / name; end
  def doc; share / "doc" / name; end
  def info; share / "info"; end
  def man; share / "man"; end
  (1..8).each { |n| define_method("man#{n}") { man / "man#{n}" } }
  def frameworks; prefix / "Frameworks"; end
  def elisp; share / "emacs" / "site-lisp" / name; end
  def bash_completion; prefix / "etc" / "bash_completion.d"; end
  def zsh_completion; share / "zsh" / "site-functions"; end
  def fish_completion; share / "fish" / "vendor_completions.d"; end
  def pwsh_completion; share / "pwsh" / "completions"; end
  def zsh_function; share / "zsh" / "site-functions"; end
  def fish_function; share / "fish" / "vendor_functions.d"; end
  def etc; Pathname.new(HOMEBREW_PREFIX) / "etc"; end
  def var; Pathname.new(HOMEBREW_PREFIX) / "var"; end
  def opt_prefix; Pathname.new(HOMEBREW_PREFIX) / "opt" / name; end
  %w[bin sbin lib libexec include share pkgshare frameworks].each do |dir|
    define_method("opt_#{dir}") do
      dir == "pkgshare" ? opt_prefix / "share" / name : opt_prefix / dir
    end
  end
  def rpath(source: bin, target: lib)
    base = OS.mac? ? "@loader_path" : "$ORIGIN"
    "#{base}/#{target.relative_path_from(source)}"
  end

  def resource(name)
    self.class.resources.fetch(name) { raise "unknown resource #{name}" }
  end

  def resources
    self.class.resources.values
  end

  def system(command, *args)
    argv = [command, *args].flatten.map(&:to_s)
    $stderr.puts("==> #{argv.join(" ")}")
    ok = argv.length == 1 ? Kernel.system(argv.first) : Kernel.system(*argv)
    raise "Failed executing: #{argv.join(" ")}" unless ok
  end

  def quiet_system(command, *args)
    Kernel.system(*[command, *args].flatten.map(&:to_s), out: File::NULL, err: File::NULL)
  end

  def safe_popen_read(*command); Utils.safe_popen_read(*command); end

  def inreplace(paths, before = nil, after = nil, audit_result: true, global: true)
    Array(paths).each do |path|
      text = WaxInreplaceString.new(File.read(path.to_s))
      original = text.dup
      if block_given?
        yield text
      elsif global
        text.gsub!(before, after.to_s)
      else
        text.sub!(before, after.to_s)
      end
      raise "inreplace failed for #{path}" if audit_result && text == original
      File.write(path.to_s, text)
    end
  end

  def std_cargo_args(root: prefix, path: ".")
    ["--locked", "--root=#{root}", "--path=#{path}"]
  end

  def std_go_args(output: bin / name, ldflags: nil, gcflags: nil, tags: nil)
    args = ["-trimpath", "-o=#{output}"]
    args << "-tags=#{Array(tags).join(" ")}" if tags
    args << "-ldflags=#{Array(ldflags).join(" ")}" if ldflags
    args << "-gcflags=#{Array(gcflags).join(" ")}" if gcflags
    args
  end

  def std_cmake_args(install_prefix: prefix, install_libdir: "lib", find_framework: "LAST")
    %W[
      -DCMAKE_INSTALL_PREFIX=#{install_prefix} -DCMAKE_INSTALL_LIBDIR=#{install_libdir}
      -DCMAKE_BUILD_TYPE=Release -DCMAKE_FIND_FRAMEWORK=#{find_framework}
      -DCMAKE_VERBOSE_MAKEFILE=ON -DBUILD_TESTING=OFF -Wno-dev
    ]
  end

  def std_configure_args(prefix: self.prefix, libdir: "lib")
    ["--disable-debug", "--disable-dependency-tracking", "--prefix=#{prefix}", "--libdir=#{prefix}/#{libdir}"]
  end

  def std_meson_args
    ["--prefix=#{prefix}", "--libdir=#{lib}", "--buildtype=release", "--wrap-mode=nofallback"]
  end

  def std_npm_args(prefix: libexec)
    ["--global", "--build-from-source", "--prefix=#{prefix}"]
  end

  def std_pip_args(prefix: self.prefix, build_isolation: false)
    args = ["--verbose", "--no-deps", "--no-binary=:all:", "--ignore-installed", "--no-compile"]
    args << "--prefix=#{prefix}" if prefix
    args << "--no-build-isolation" unless build_isolation
    args
  end

  def generate_completions_from_executable(*commands, base_name: nil, shells: %i[bash zsh fish],
                                           shell_parameter_format: nil)
    base_name ||= name
    targets = {
      bash: bash_completion / base_name,
      zsh: zsh_completion / "_#{base_name}",
      fish: fish_completion / "#{base_name}.fish",
      pwsh: pwsh_completion / "_#{base_name}.ps1"
    }
    shells.each do |shell|
      argument = case shell_parameter_format
                 when nil then shell.to_s
                 when :flag then "--#{shell}"
                 when :arg then "--shell=#{shell}"
                 when :none then nil
                 when :clap then nil
                 when :cobra then shell.to_s
                 when :click then nil
                 else "#{shell_parameter_format}#{shell}"
                 end
      env = {}
      env["COMPLETE"] = shell.to_s if shell_parameter_format == :clap
      env["_#{base_name.upcase.tr("-", "_")}_COMPLETE"] = "#{shell}_source" if shell_parameter_format == :click
      command = [*commands, argument].compact.map(&:to_s)
      output, status = Open3.capture2(env, *command)
      next unless status.success? && !output.empty?
      targets.fetch(shell).dirname.mkpath
      targets.fetch(shell).write(output)
    end
  end

  def ohai(*message); $stderr.puts("==> #{message.join(" ")}"); end
  def opoo(*message); $stderr.puts("Warning: #{message.join(" ")}"); end
  def odebug(*); end
  def post_install; end
  def caveats; nil; end

  def method_missing(name, *_args)
    raise NoMethodError, "wax does not support Formula##{name} yet"
  end

  def respond_to_missing?(*); false; end
end

def wax_formula_meta(klass)
  spec = klass.wax
  {
    "url" => spec[:url],
    "sha256" => spec[:sha256],
    "version" => spec[:version],
    "using" => spec[:using],
    "head" => spec[:head],
    "dependencies" => spec[:dependencies].uniq,
    "build_dependencies" => spec[:build_dependencies].uniq,
    "keg_only" => spec[:keg_only] ? true : false,
    "patches" => spec[:unsupported_patch] ? true : false
  }
end
