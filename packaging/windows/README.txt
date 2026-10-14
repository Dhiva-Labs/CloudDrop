CloudDrop for Windows - test build
==================================

Install
  Double-click install.cmd. CloudDrop installs for your Windows user only (no
  administrator rights) into %LOCALAPPDATA%\Programs\CloudDrop, adds a Start
  menu entry and opens the setup assistant. Running install.cmd again upgrades.

Sign in
  Builds from GitHub have no Google sign-in key built in. In that case the setup
  assistant asks for one: pick the client_secret_....json you downloaded from
  Google Cloud for CloudDrop (the assistant shows how to make one).

What to try
  - Put files in the CloudDrop folder (File Explorer > CloudDrop in the left pane).
    They upload to My Drive > CloudDrop with the same folders.
  - Right-click a file > "Free up space": it stays listed with a cloud icon and
    downloads again when you open it. Deleting a file never deletes it in Drive.
  - The CloudDrop icon in the notification area shows progress and can pause.

Uninstall
  Settings > Apps > Installed apps > CloudDrop > Uninstall. Nothing is deleted
  from Google Drive. Files with a copy on this PC stay in the CloudDrop folder.

Smart App Control
  This build isn't signed. If Windows Smart App Control is on, it may refuse to
  run it; the Microsoft Store build is signed by Microsoft and isn't affected.
