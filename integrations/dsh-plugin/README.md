# Open Console Gateway for DSH

This package is generated and installed by the Open Console Gateway Desktop
application. It registers one `open-console-gateway` provider in the DSH `web`
profile. The provider reads the current authenticated `GET /v1/models` catalog
from the local Gateway and forwards model calls through the OpenAI-compatible
Chat Completions endpoint.

The installer hands the selected Gateway Key to DSH through a private,
one-time live file. This package is activated once per DSH runtime. On
activation the plugin claims that live file by rename, imports the value,
and deletes every leftover claim file so a newer live handoff is never
unlinked and a stale claim cannot keep activation pending. If credential
storage fails and no newer live file exists, the claim is restored for
retry.

Do not copy or edit this generated package by hand. Re-run the installer from
the **Applications > DSH** page when repair is required.
