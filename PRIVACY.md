# CloudDrop privacy policy

*Last updated 29 October 2026*

CloudDrop is a desktop application for Linux and Windows by Dhiva Labs that uploads the files you
put in a folder on your computer to your own cloud storage: Google Drive, OneDrive, Dropbox, a
Nextcloud or other WebDAV server, S3-compatible storage, an SFTP server or MEGA. This policy
explains how it uses Google user data and the data of the other clouds you connect it to. The
general Dhiva Labs policy is at <https://dhivalabs.com/privacy.html>.

## Google Drive

### The one permission it asks for

When you sign in, CloudDrop requests a single Google permission,
`https://www.googleapis.com/auth/drive.file`. It lets CloudDrop see, create, update
and download only the files and folders that CloudDrop itself creates in your Drive.
CloudDrop cannot see any other file in your Drive.

### What it accesses

- The files you place in your CloudDrop folder.
- The copies of them it creates in your Drive, including their names, sizes and
  checksums, which it uses to confirm an upload is complete before freeing space on
  your computer.
- Your Drive storage usage, which it shows you in the app.

### Where your data goes

Directly between your computer and Google's servers. Dhiva Labs runs no server for
CloudDrop and never receives your files, file names, sign-in tokens or any other
Google user data.

### What is stored on your computer

Your Google sign-in token, in a file in your user account's private app-data folder,
and a local database recording what has been uploaded and an activity history. Both
stay on your computer.

### No sharing or other use

CloudDrop does not sell, share or transfer Google user data to anyone, does not use
it for advertising, and does not use it to develop, improve or train AI or
machine-learning models.

### Removing access

Sign out in CloudDrop's Settings, or revoke access at any time at
<https://myaccount.google.com/permissions>. Uninstalling CloudDrop does not delete
anything from your Drive.

### Limited Use

CloudDrop's use and transfer to any other app of information received from Google
APIs will adhere to the
[Google API Services User Data Policy](https://developers.google.com/terms/api-services-user-data-policy),
including the Limited Use requirements.

## Other clouds

The same rules apply to every other cloud you connect CloudDrop to.

- **What it asks for.** As little as each cloud allows. On OneDrive (Microsoft Graph,
  `Files.ReadWrite.AppFolder` and `offline_access`) and on Dropbox (an *App folder* app), CloudDrop
  can reach only its own folder, `Apps/CloudDrop`, and none of your other files. Nextcloud, WebDAV,
  S3-compatible storage, SFTP and MEGA give an app the account or key you sign in with; CloudDrop
  works only inside the folder, bucket prefix or directory you choose.
- **What it accesses.** The files you place in your CloudDrop folder, the copies it creates in your
  cloud with their names, sizes and checksums, and your storage usage where the cloud reports it.
- **Where your data goes.** Directly between your computer and the cloud you chose. Dhiva Labs
  runs no server for CloudDrop and never receives your files, file names, passwords, keys,
  sign-in tokens or any other data from your cloud.
- **What is stored on your computer.** Your sign-in tokens, app passwords or keys, in files in your
  user account's private app-data folder that only you can read, and the local database described
  above. They stay on your computer.
- **No sharing or other use.** CloudDrop does not sell, share or transfer this data to anyone, does
  not use it for advertising, and does not use it to develop, improve or train AI or
  machine-learning models.
- **Removing access.** Sign out in CloudDrop's Settings or remove the folder with *Remove This
  Folder…*, and revoke CloudDrop's access in your account (Microsoft:
  <https://account.live.com/consent/Manage>; Dropbox:
  <https://www.dropbox.com/account/connected_apps>), or delete the app password or key you made
  for it. Uninstalling CloudDrop does not delete anything from your cloud.

Proton Drive, offered only by builds from source, goes through Proton's own command-line app,
which signs in and holds your Proton session itself; CloudDrop never sees your Proton password.

## Contact

Questions: <reachout@dhivalabs.com> · Dhiva Labs, Chennai, India
