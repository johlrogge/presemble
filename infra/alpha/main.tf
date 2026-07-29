# ---------------------------------------------------------------------------
# Lookups for things managed outside this config.
# The networks are Bahnhof-pre-provisioned.
# The keypair was uploaded once (managed by infra/openstack); we look it up
# here with a data source so both modules share it without collision.
# ---------------------------------------------------------------------------

data "openstack_images_image_v2" "debian" {
  name        = var.image_name
  most_recent = true
}

data "openstack_networking_network_v2" "tenant" {
  name = var.network_name
}

data "openstack_networking_network_v2" "external" {
  name     = var.external_network_name
  external = true
}

# Reference the existing presemble keypair — do NOT re-create or manage it.
# It lives in infra/openstack's state; here we look it up read-only.
data "openstack_compute_keypair_v2" "presemble" {
  name = var.keypair_name
}

# ---------------------------------------------------------------------------
# Security group: ssh / http / https in, default allow-all out.
# Ports 2019 (Caddy admin) and 3100-3999 (per-account conductors) are
# deliberately NOT listed here — they must only be reachable on localhost.
# ---------------------------------------------------------------------------

resource "openstack_networking_secgroup_v2" "presemble_alpha" {
  name        = "presemble-alpha"
  description = "presemble alpha: ssh, http, https only — admin/conductor ports are localhost-only"
}

locals {
  ingress = {
    ssh   = { port = 22, cidr = var.ssh_allowed_cidr }
    http  = { port = 80, cidr = "0.0.0.0/0" }
    https = { port = 443, cidr = "0.0.0.0/0" }
  }
}

resource "openstack_networking_secgroup_rule_v2" "presemble_alpha_ingress" {
  for_each = local.ingress

  direction         = "ingress"
  ethertype         = "IPv4"
  protocol          = "tcp"
  port_range_min    = each.value.port
  port_range_max    = each.value.port
  remote_ip_prefix  = each.value.cidr
  security_group_id = openstack_networking_secgroup_v2.presemble_alpha.id
}

# ---------------------------------------------------------------------------
# Port on the tenant network. Holds the security group and gives the floating
# IP a stable attachment point.
# ---------------------------------------------------------------------------

resource "openstack_networking_port_v2" "presemble_alpha" {
  name               = "presemble-alpha-port"
  network_id         = data.openstack_networking_network_v2.tenant.id
  admin_state_up     = true
  security_group_ids = [openstack_networking_secgroup_v2.presemble_alpha.id]
}

# ---------------------------------------------------------------------------
# Alpha instance — boot from a volume built from the Debian image.
# ---------------------------------------------------------------------------

resource "openstack_compute_instance_v2" "presemble_alpha" {
  name              = var.instance_name
  flavor_name       = var.flavor_name
  key_pair          = data.openstack_compute_keypair_v2.presemble.name
  availability_zone = var.availability_zone
  user_data = templatefile("${path.module}/cloud-init.yaml", {
    presemble_session_key = var.presemble_session_key
  })

  network {
    port = openstack_networking_port_v2.presemble_alpha.id
  }

  block_device {
    uuid                  = data.openstack_images_image_v2.debian.id
    source_type           = "image"
    destination_type      = "volume"
    volume_size           = var.volume_size
    boot_index            = 0
    delete_on_termination = true
  }

  # Editing cloud-init should not force-replace a running box; re-provisioning
  # is a deliberate act (taint + apply), not a side-effect of a docs tweak.
  lifecycle {
    ignore_changes = [user_data]
  }
}

# ---------------------------------------------------------------------------
# Floating IP — public address, associated with the instance.
# ---------------------------------------------------------------------------

resource "openstack_networking_floatingip_v2" "presemble_alpha" {
  pool = data.openstack_networking_network_v2.external.name

  # prevent_destroy is intentionally left OUT for initial provisioning so the
  # resource can be cleanly destroyed if first-boot goes wrong.
  # ENABLE THIS after the first successful deploy + DNS wiring:
  #
  #   lifecycle {
  #     prevent_destroy = true
  #   }
}

resource "openstack_networking_floatingip_associate_v2" "presemble_alpha" {
  floating_ip = openstack_networking_floatingip_v2.presemble_alpha.address
  port_id     = openstack_networking_port_v2.presemble_alpha.id
}
