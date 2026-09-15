# Music

Drop audio files in this folder and they play during a race, in filename order,
looping back to the first when the last one ends. Name them `01-...`, `02-...`
if you want a particular running order.

Accepted: `.mp3`, `.ogg`, `.flac`, `.wav`, `.m4a`.

With this folder empty the game generates its own music instead, so it is never
silent while you are still choosing tracks.

Tracks are decoded whole into memory at startup, at roughly 35 MB per three
minutes. A handful is fine on the target board; an hour of music would want
streaming instead of this.

The MUSIC slider on the audio menu sets the level.
