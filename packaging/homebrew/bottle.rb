#!/usr/bin/env ruby
# frozen_string_literal: true

# Add a `bottle do` block to a rendered formula (render.sh's output) from the
# JSON files `brew bottle --json` wrote, one per macOS runner.
# .github/workflows/homebrew.yml runs this after the bottle jobs; with no JSON
# files it prints the formula unchanged, which builds from source as before.
#
#   packaging/homebrew/bottle.rb FORMULA ROOT_URL [BOTTLE.json...] > Formula/ssf.rb
require "json"

abort "usage: #{$PROGRAM_NAME} FORMULA ROOT_URL [BOTTLE.json...]" if ARGV.length < 2
formula_path, root_url, *json_paths = ARGV
formula = File.read(formula_path)
abort "bottle.rb: #{formula_path} already has a bottle block" if formula.include?("\n  bottle do\n")

version = formula[/^  url "[^"]+\/v(\d+\.\d+\.\d+)\.tar\.gz"$/, 1]
abort "bottle.rb: no tag tarball url in #{formula_path}" unless version

lines = []
json_paths.each do |path|
  JSON.parse(File.read(path)).each_value do |entry|
    got = entry.dig("formula", "pkg_version")
    abort "bottle.rb: #{path} is for #{got}, the formula for #{version}" if got != version
    bottle = entry.fetch("bottle")
    abort "bottle.rb: #{path} is a rebuild; not supported" unless bottle["rebuild"].to_i.zero?
    cellar = bottle.fetch("cellar")
    cellar = cellar.start_with?(":") ? cellar : cellar.inspect
    bottle.fetch("tags").each do |tag, info|
      sha = info.fetch("sha256")
      abort "bottle.rb: '#{sha}' is not a sha256" unless sha.match?(/\A[0-9a-f]{64}\z/)
      lines << "    sha256 cellar: #{cellar}, #{tag}: \"#{sha}\""
    end
  end
end

if lines.empty?
  print formula
  exit
end

block = "  bottle do\n    root_url \"#{root_url}\"\n#{lines.sort.uniq.join("\n")}\n  end\n\n"
# Homebrew's order puts the bottle block before the dependencies.
at = formula.index(/^  depends_on /) or abort "bottle.rb: no depends_on line in #{formula_path}"
print formula.dup.insert(at, block)
