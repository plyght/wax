import sys

with open('src/tap.rs', 'r') as f:
    content = f.read()

find = 'Err(crate::error::WaxError::TapError(format!("Failed to parse formula {}: {}", name, e)))'
replace = 'Err(crate::error::WaxError::ParseError(format!("Failed to parse formula {}: {}", name, e)))'

if find in content:
    content = content.replace(find, replace)
    print("Replaced TapError with ParseError")
else:
    print("Could not find TapError")

with open('src/tap.rs', 'w') as f:
    f.write(content)
