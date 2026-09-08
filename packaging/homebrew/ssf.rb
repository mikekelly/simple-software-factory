# Homebrew formula for ssf (macOS, Linuxbrew): the source of truth lives in
# this repository as packaging/homebrew/ssf.rb; .github/workflows/homebrew.yml
# renders it on every vX.Y.Z tag (tag tarball url and its sha256, see
# render.sh) and pushes the result to the tap mikekelly/homebrew-ssf as
# Formula/ssf.rb, where `brew install mikekelly/ssf/ssf` finds it.
#
# Differences from the Arch package (packaging/release/PKGBUILD): no ssf-ui
# and no Omarchy bar widget (both Omarchy-only), no systemd unit (the
# `service` block below is the launchd agent), and no herdr dependency: the
# factory runs in a lima VM on macOS and herdr lives in the guest.
class Ssf < Formula
  desc "GitHub issues assigned to a bot become coding-agent sessions in herdr or Orca"
  homepage "https://github.com/mikekelly/simple-software-factory"
  url "https://github.com/mikekelly/simple-software-factory/archive/refs/tags/v0.1.0.tar.gz"
  # Placeholder: filled in by .github/workflows/homebrew.yml on each tag.
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  license "MIT"
  head "https://github.com/mikekelly/simple-software-factory.git", branch: "master"

  livecheck do
    url :stable
    strategy :github_latest
  end

  depends_on "rust" => :build
  depends_on "gh"
  depends_on "lima"

  def install
    # Cargo.toml has one binary target, ssf; bin/ssf-ui is the Omarchy bar
    # widget's helper and is not installed here.
    system "cargo", "install", *std_cargo_args

    # #{HOMEBREW_PREFIX}/share/ssf/vm is where `ssf vm build` looks for the
    # image scripts, relative to the binary (<bin>/../share/ssf/vm).
    pkgshare.install "vm", "config.example.toml", "SSF.example.md"
    doc.install "README.md", "docs"
  end

  def caveats
    <<~EOS
      Setup, step by step (also for your coding agent to follow):
        #{HOMEBREW_PREFIX}/share/doc/ssf/docs/setup.md

      On macOS the factory runs in a lima VM: build it once with
        ssf vm build
      then start the service, which supervises the VM and the daemon:
        brew services start ssf
      The log is #{var}/log/ssf.log.

      To run the factory on this machine instead of in a VM (driver = "herdr"
      with [vm] enabled = false), or to attach to a session with `herdr
      --remote`, install herdr on the host:
        brew install herdr
    EOS
  end

  service do
    run [opt_bin/"ssf", "run"]
    keep_alive true
    # SSF_VM_DIR: where `ssf run` finds the VM scripts (also found relative
    # to the binary; this keeps the service independent of that lookup).
    environment_variables PATH:       std_service_path_env,
                          RUST_LOG:   "info",
                          SSF_VM_DIR: opt_pkgshare/"vm"
    log_path var/"log/ssf.log"
    error_log_path var/"log/ssf.log"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/ssf --version")
    # Runs without a config file or a login: lists the known coding agents.
    assert_match "Claude Code", shell_output("#{bin}/ssf agents")
  end
end
