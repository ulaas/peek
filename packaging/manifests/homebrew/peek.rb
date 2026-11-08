# Homebrew cask for a tap: brew install --cask <owner>/<tap>/peek
cask "peek" do
  version "@VERSION@"
  sha256 "@SHA256_DMG@"

  url "https://github.com/ulaas/peek/releases/download/v#{version}/peek-macos-arm64.dmg"
  name "Peek"
  desc "Fast, quiet RDP and VNC remote desktop client"
  homepage "https://ulaas.github.io/peek/"

  livecheck do
    url :url
    strategy :github_latest
  end

  depends_on arch: :arm64
  depends_on macos: ">= :big_sur"

  app "Peek.app"
  binary "#{appdir}/Peek.app/Contents/MacOS/peek"

  # Settings, library and known hosts (directories::ProjectDirs "dev", "Peek", "Peek").
  # Recordings in ~/Movies/Peek are left alone.
  zap trash: "~/Library/Application Support/dev.Peek.Peek"

  caveats <<~EOS
    Peek is not notarized yet. If macOS blocks the first launch, run:
      xattr -dr com.apple.quarantine "#{appdir}/Peek.app"
  EOS
end
