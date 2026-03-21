use regex::Regex;

fn main() {
    let ruby = r#"
  preflight do
    File.write shimscript, <<~EOS
      #!/bin/bash
      exec '#{appdir}/Firefox.app/Contents/MacOS/firefox' "$@"
    EOS
  end
        "#;
    
    let re = Regex::new(r"(?ms)File\.write\s+shimscript,\s*<<~(?P<delim>[A-Z_]+)\n(?P<script>.*?)\n\s*(?P=delim)").unwrap();
    if let Some(cap) = re.captures(ruby) {
        println!("Match: {:?}", &cap["script"]);
    } else {
        println!("No match!");
    }
}
