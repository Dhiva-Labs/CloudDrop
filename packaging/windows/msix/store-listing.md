# Microsoft Store listing (Partner Center)

The text entered in Partner Center for CloudDrop, kept here for future updates.

## Pricing and availability

- Price: free. No in-app purchases.
- Organizational licensing: Store-managed (online) allowed.
- Markets: all.
- Visibility: public.

## Properties

- Category: Productivity.
- Privacy policy: <https://github.com/Dhiva-Labs/CloudDrop/blob/main/PRIVACY.md>
- Website: <https://github.com/Dhiva-Labs/CloudDrop>
- Support contact: reachout@dhivalabs.com
- Product declarations: installs to alternate drives allowed; OneDrive backup of app data turned OFF (it would copy the Google sign-in token off the PC, and the privacy policy says it stays on the computer).
- System requirements: an internet connection and a Google account.

## Store listing (English, United States)

**Product name:** CloudDrop

**Short description:**

Put files in a folder and they go to your Google Drive, folders and all. Free up space on your PC without losing anything: deleting locally never deletes in Drive.

**Description:**

CloudDrop gives you a CloudDrop folder in File Explorer that uploads everything you put in it to your Google Drive, into a CloudDrop folder with the same structure.

It's made for saving space. Uploading is one-way: deleting or freeing up a file on your PC never deletes it from Drive. CloudDrop keeps it listed, so you can download it back whenever you want.

• A real File Explorer cloud folder. CloudDrop appears in the navigation pane with Explorer's own status icons. Uploaded files get the green check.
• Free up space in one click. Right-click > Free up space keeps a file listed with a cloud icon but removes its local data, and opening it downloads it again. CloudDrop removes local data only after checking that Google Drive holds an identical copy.
• Automatic space saving. Free local copies right after upload, or after 7, 30 or 90 days, with folders you mark "Always keep on this device" left alone.
• Keeps your folders. Renames and moves are applied in Drive without uploading again, and edits become new versions of the same file.
• Never loses an upload. Large files resume where they stopped after a network drop, a restart or a crash.
• A clear app. A setup assistant, a Home overview with your Drive storage and the space you've freed, a Files browser with search and filters, an Activity history, and Settings, plus a notification-area icon with pause and resume.
• Private by design. CloudDrop asks Google only for access to the files it creates itself (the drive.file permission), so it cannot see the rest of your Drive. Your files go straight between your PC and Google: no CloudDrop servers.

CloudDrop is open source (MIT) at github.com/Dhiva-Labs/CloudDrop. The Store edition is free and ready to use: signed, updated by the Store, with Google sign-in built in.

**What's new in this version:** First Windows release.

**Product features:**

1. File Explorer cloud folder with status icons
2. Uploads to Google Drive with your folder structure
3. Deleting or freeing locally never deletes in Drive
4. Free up space by hand or automatically
5. Checksum-verified before any local copy is removed
6. Resumable uploads for large files
7. Download cloud-only files back any time
8. Only accesses files it creates in your Drive

**Keywords:** google drive, backup, upload, free up space, file sync, drive uploader, cloud backup (Partner Center rejects "cloud storage")

**Copyright:** © 2026 Dhiva Labs

**Developed by:** Dhiva Labs

**Screenshots:** `winapp/store/screenshots/*.png`

**Store logos:** `packaging/windows/msix/store-logos/` (9:16 poster art 1440×2160, 1:1 box art 1080×1080, 1:1 app tile icon 300×300), rendered by `packaging/make-icons.py`.

## Age ratings (IARC)

All Other App Types; every question answered No (no ratings-relevant content, no user-to-user sharing, no featured online content, no age-restricted products, no location sharing, no digital purchases, no rewards, not a browser, not news/education). Result: IARC 3+, ESRB Everyone, PEGI 3, USK 0.

## Certification notes

CloudDrop uploads the contents of %USERPROFILE%\CloudDrop to the tester's own Google Drive. Any free Google account works. CloudDrop requests only the drive.file permission, so it sees nothing else in the account.

To test it:
1. Open CloudDrop and click through the setup assistant to Sign in. A browser opens for Google sign-in.
2. Copy a few files into the CloudDrop folder (File Explorer > CloudDrop in the navigation pane). They upload, and Explorer shows a green check on each.
3. Right-click a file > Free up space. It stays listed with a cloud icon, and opening it downloads it again.
4. The notification-area icon shows progress and can pause.

The background agent (clouddrop-agent.exe) is the package's startup task. It runs the upload engine and the File Explorer cloud-folder provider (Cloud Files API). runFullTrust is needed for the Cloud Files API and the notification-area icon.
