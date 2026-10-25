cask "toolport" do
  version "2.3.1"

  on_arm do
    sha256 "4cd0e1608e586227cecffbbfa4fe0792230c4f1170d2e2c82be69463e22235c3"
    url "https://github.com/btsouth/toolport/releases/download/v#{version}/Toolport_aarch64-apple-darwin.dmg"
  end
  on_intel do
    sha256 "b077bab97912a907437fc686aa9cab0c590f2dc3a1e249cab3ef3cc665409c4c"
    url "https://github.com/btsouth/toolport/releases/download/v#{version}/Toolport_x86_64-apple-darwin.dmg"
  end

  name "Toolport"
  desc "One local gateway for every MCP server, shared by every AI client"
  homepage "https://toolport.app/"

  # livecheck reports the latest GitHub tag for `brew livecheck`. brew install
  # and brew upgrade still use the pinned version + sha256 above. Bump those
  # on each published release (see docs/RELEASING.md).
  livecheck do
    url :url
    strategy :github_latest
  end

  app "Toolport.app"

  # Homebrew also runs uninstall_preflight on upgrades without exposing the
  # upgrade flag to that block. Disconnecting there would break live clients.
  caveats <<~EOS
    Before brew uninstall or zap, open Toolport Settings and choose
    "Remove Toolport from all clients". Keep Toolport's data if cleanup fails.
    Or run "#{appdir}/Toolport.app/Contents/MacOS/toolport-gateway" --disconnect-all
    as your normal user while the app is still installed.
    Uninstall keeps data and credentials. Zap removes app caches and preferences,
    but keeps the registry, recovery data and credentials. To remove them, choose
    "Remove Toolport data" in Settings before uninstalling.
  EOS

  # Explicit zap removes only app-owned storage, never client recovery data.
  zap trash: [
    "~/Library/Caches/com.tsout.conduit",
    "~/Library/HTTPStorages/com.tsout.conduit",
    "~/Library/Preferences/com.tsout.conduit.plist",
    "~/Library/Saved Application State/com.tsout.conduit.savedState",
    "~/Library/WebKit/com.tsout.conduit",
  ]
end
