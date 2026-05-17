# Homebrew formula for `tad`. Drop into the tap repo at
# github.com/ttpears/homebrew-tad as Formula/tad.rb, then users run:
#
#   brew tap ttpears/tad
#   brew install tad
#
# Bump `url` + `sha256` for each release. Get the hash with:
#   curl -sL https://github.com/ttpears/tad/archive/refs/tags/vX.Y.Z.tar.gz | sha256sum
class Tad < Formula
  desc "Tmux session and group manager with a native TUI dashboard"
  homepage "https://github.com/ttpears/tad"
  url "https://github.com/ttpears/tad/archive/refs/tags/v0.6.0.tar.gz"
  sha256 "88aaa413c5d6dd3a4fcfd3739affa3446638c0356dcfb2d6b95044ec7d983070"
  license "MIT"
  head "https://github.com/ttpears/tad.git", branch: "main"

  depends_on "rust" => :build
  depends_on "tmux"

  def install
    system "cargo", "install", *std_cargo_args
    bash_completion.install "completions/tad.bash" => "tad"
    zsh_completion.install "completions/_tad"
    pkgshare.install "examples/groups.yaml.example",
                     "examples/config.yaml.example"
  end

  test do
    assert_match "tad #{version}", shell_output("#{bin}/tad --version")
  end
end
