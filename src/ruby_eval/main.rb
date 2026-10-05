kind, path, $wax_hook = ARGV
valid = path && (kind == "cask" || (%w[hook formula].include?(kind) && $wax_hook))
abort("usage: shim.rb cask <file> | hook <file> <name> | formula <file> meta|install|post_install") unless valid
$wax_formula_mode = kind == "formula" ? $wax_hook : nil
$wax_hook = nil unless kind == "hook"

begin
  load File.expand_path(path)
  if $wax_formula_mode
    klass = $wax_formula_class
    raise "no formula class found in #{path}" unless klass
    case $wax_formula_mode
    when "meta"
      $wax_real_stdout.puts("__WAX_JSON__#{JSON.generate(wax_formula_meta(klass))}")
    when "install"
      formula = klass.new
      Dir.chdir(formula.buildpath.to_s) { formula.install }
      formula.prefix.mkpath
      $wax_real_stdout.puts("__WAX_HOOK_OK__")
    when "post_install"
      formula = klass.new
      pid = fork do
        $stdout = $stderr
        Dir.chdir(formula.prefix.to_s) { formula.post_install }
        exit!(0)
      end
      Process.wait(pid)
      raise "post_install exited with #{$?.exitstatus}" unless $?.success?
      $wax_real_stdout.puts("__WAX_HOOK_OK__")
    else
      raise "unknown formula mode #{$wax_formula_mode}"
    end
    $wax_real_stdout.flush
    exit!(0)
  end
rescue SystemExit
  raise
rescue Exception => e
  $wax_real_stdout.puts("__WAX_ERROR__#{e.class}: #{e.message}")
  $wax_real_stdout.flush
  exit!(1)
end
$wax_real_stdout.puts("__WAX_ERROR__no cask definition found in #{path}")
exit!(1)
