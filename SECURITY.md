# Security policy

EH3D-SERVER takes jobs from programs and from people and can start a machine, so what it accepts is checked before anything runs.

- **Who may do what**: every request except the health check, the first set-up and the login needs a token. A viewer reads; an operator also prepares, starts, stops and confirms jobs; an administrator also manages users, tokens and machines.
- **Passwords** are kept as Argon2 hashes; session and program tokens are random and only their SHA-256 is kept, so a copy of the data folder does not log anybody in. A wrong password is slowed down.
- **The token of a node** (the access token of its control link) is kept in the server's data folder and is never shown by the API.
- **A job is checked before it is planned**: holes and paths outside the table, tools that are missing or out of range, and oversized requests are refused with a code. A job never starts by itself: a person starts it.
- **The server is for the house** (or a VPN into it). Put it behind HTTPS (a reverse proxy) before it is reachable from anywhere else; it does not terminate TLS itself yet.

Report a vulnerability privately to electrohobby3d@gmail.com. Please do not open a public issue for it.
