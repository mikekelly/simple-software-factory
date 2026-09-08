# Homebrew packaging

`ssf.rb` is the Homebrew formula for ssf and the only copy that is edited.
It is written for the version in `Cargo.toml` with a placeholder sha256;
on every `vX.Y.Z` tag, `.github/workflows/homebrew.yml` runs `render.sh`
to put the tag tarball's url and sha256 in (the source of truth keeps the
placeholder) and pushes the result to the tap repository
[mikekelly/homebrew-ssf](https://github.com/mikekelly/homebrew-ssf) as
`Formula/ssf.rb`, which is what `brew install` reads.

## Installing

```sh
brew install mikekelly/ssf/ssf
```

(`mikekelly/ssf/ssf` is Homebrew's short name for `Formula/ssf.rb` in the
`mikekelly/homebrew-ssf` repository; `brew tap mikekelly/ssf` first is
equivalent.) The formula's caveats say what comes next: setup document,
`ssf vm build`, `brew services start ssf`.

## The tap, once

The maintainer creates the tap repository and gives this repository's
workflow a token that can push to it:

1. `gh repo create mikekelly/homebrew-ssf --public` (a tap has to be named
   `homebrew-<name>`), then commit a first `Formula/ssf.rb` to it, for
   example the workflow artifact of the current release, or a render made
   by hand:

   ```sh
   v=0.1.0
   curl -fsSLo ssf.tar.gz "https://github.com/mikekelly/simple-software-factory/archive/refs/tags/v$v.tar.gz"
   packaging/homebrew/render.sh "$v" "$(sha256sum ssf.tar.gz | cut -d' ' -f1)" > <tap>/Formula/ssf.rb
   ```

2. Make a fine-grained personal access token on GitHub with *Contents:
   read and write* on `mikekelly/homebrew-ssf` only, and save it as the
   `HOMEBREW_TAP_TOKEN` Actions secret of this repository
   (`gh secret set HOMEBREW_TAP_TOKEN`). Without the secret the workflow
   still renders the formula and uploads it as a workflow artifact, says
   so, and does not fail.

The tap repository's name is the `HOMEBREW_TAP` environment variable at the
top of the workflow; nothing else needs to know it besides the header
comment of the formula and the install command above.

The tag tarball has to be downloadable without a login, or Homebrew cannot
fetch the source: this repository must be public for the formula to work
(the workflow's download of the tarball fails with a 404 while it is
private).

## Testing a formula change

On a Mac with Homebrew, from the repository root:

```sh
brew install --build-from-source ./packaging/homebrew/ssf.rb
brew audit --strict --new ssf
brew test ssf
brew services start ssf   # the launchd agent; `brew services stop ssf` to stop it
```

`brew install` from a local file checks the `url` download against the
`sha256` in it, so the placeholder fails; `--HEAD` builds the `master`
branch from git without a sha256, and to test the stable url render the
formula first (the command block above) and install the rendered file.
`brew audit --strict` flags the placeholder sha256 too; the rendered copy
in the tap is what should pass it clean.
