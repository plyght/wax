import sys

with open('src/tap.rs', 'r') as f:
    content = f.read()

find1 = """                    Err(e) => {
                        debug!("Failed to parse formula {}: {}", path.display(), e);
                        Ok(Vec::new())
                    }"""

replace1 = """                    Err(e) => {
                        debug!("{}", e);
                        Ok(Vec::new())
                    }"""

find2 = """                            Err(e) => {
                                debug!("Failed to parse formula {}: {}", path.display(), e);
                            }"""

replace2 = """                            Err(e) => {
                                debug!("{}", e);
                            }"""

content = content.replace(find1, replace1)
content = content.replace(find2, replace2)

with open('src/tap.rs', 'w') as f:
    f.write(content)
