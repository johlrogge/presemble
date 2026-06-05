# Presemble infrastructure — Bahnhof OpenStack

OpenTofu config that provisions the presemble conductor VPC: security group,
boot-from-volume Debian instance, and a floating IP. The in-box setup
(hardening, Caddy) is captured in `cloud-init.yaml` so a fresh instance
self-configures on first boot.

## Prerequisites

- `opentofu` and `openstackclient` (provided by devenv).
- Credentials at `~/.config/openstack/clouds.yaml`, cloud entry named `openstack`
  (see `var.os_cloud`). **Never commit credentials.** Each Bahnhof project is a
  separate VPC with its own login.
- The keypair named by `var.keypair_name` already uploaded to the project.

## Usage

```sh
cd infra/openstack
tofu init
tofu plan
tofu apply
tofu output ssh        # -> ssh debian@<floating-ip>
```

Tear the whole VPC down with `tofu destroy`.

## Notes

- **Boot from volume:** Bahnhof flavors report `Disk=0`, so instances must boot
  from a Cinder volume. The `block_device` block builds one from the image.
- **TLS:** `cloud-init.yaml` ships a stopgap Caddy config on port 80. Point a DNS
  A record at the floating IP, then replace the `Caddyfile` vhost with
  `reverse_proxy localhost:<conductor-port>` and Caddy auto-provisions a
  Let's Encrypt cert.
- **State contains secrets.** `*.tfstate` is gitignored. For a solo setup local
  state is fine; switch to a remote backend if this ever goes multi-person.
- **Reusing an existing IP after re-provisioning:** clear the stale host key with
  `ssh-keygen -R <floating-ip>`.

## Adopting the hand-built stack

The first conductor was created by hand. To bring the live resources under Tofu
without recreating them, use `import` blocks (preserves the running box and its
floating IP), then `tofu plan` until it shows no changes. Otherwise `tofu apply`
builds a parallel stack — fine when the old one is disposable.
