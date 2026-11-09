# Trusted file: sourced by a fresh noninteractive Bash for each request.
# Keep this setup lightweight; inherited PATH is preserved and extended here.
export PATH="/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:${PATH:-}"

# Add only trusted local aliases or functions that you need. They can perform
# arbitrary side effects and are not constrained by the client allow rules.
