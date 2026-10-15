# Target image overlay

Install the `usr/lib/systemd/system` unit files into the target image. The
`flowerly` and `flowerly-evolution` service accounts must be created by
packaging before units are enabled. They intentionally have different UIDs.
Do not co-locate their private Unix sockets or state directories.
