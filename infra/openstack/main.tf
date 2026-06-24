# ---------------------------------------------------------------------------
# Lookups for things managed outside this config (Bahnhof pre-provisions the
# networks; the keypair was uploaded once by hand).
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

# ---------------------------------------------------------------------------
# Security group: ssh / http / https in, default allow-all out.
# ---------------------------------------------------------------------------

resource "openstack_networking_secgroup_v2" "presemble" {
  name        = "presemble"
  description = "presemble conductor: ssh, http, https"
}

locals {
  ingress = {
    ssh   = { port = 22, cidr = var.ssh_allowed_cidr }
    http  = { port = 80, cidr = "0.0.0.0/0" }
    https = { port = 443, cidr = "0.0.0.0/0" }
  }
}

resource "openstack_networking_secgroup_rule_v2" "ingress" {
  for_each = local.ingress

  direction         = "ingress"
  ethertype         = "IPv4"
  protocol          = "tcp"
  port_range_min    = each.value.port
  port_range_max    = each.value.port
  remote_ip_prefix  = each.value.cidr
  security_group_id = openstack_networking_secgroup_v2.presemble.id
}

# ---------------------------------------------------------------------------
# SSH keypair — dedicated presemble key, managed as IaC. Only the PUBLIC key is
# uploaded; the private half lives in ~/.ssh and never enters Tofu state.
# ---------------------------------------------------------------------------

resource "openstack_compute_keypair_v2" "presemble" {
  name       = var.keypair_name
  public_key = file(pathexpand(var.ssh_public_key_path))
}

# ---------------------------------------------------------------------------
# Port on the tenant network. Holds the security group and gives the floating
# IP a stable attachment point.
# ---------------------------------------------------------------------------

resource "openstack_networking_port_v2" "conductor" {
  name               = "${var.instance_name}-port"
  network_id         = data.openstack_networking_network_v2.tenant.id
  admin_state_up     = true
  security_group_ids = [openstack_networking_secgroup_v2.presemble.id]
}

# ---------------------------------------------------------------------------
# Conductor instance — boot from a volume built from the Debian image.
# ---------------------------------------------------------------------------

resource "openstack_compute_instance_v2" "conductor" {
  name              = var.instance_name
  flavor_name       = var.flavor_name
  key_pair          = openstack_compute_keypair_v2.presemble.name
  availability_zone = var.availability_zone
  user_data         = file("${path.module}/cloud-init.yaml")

  network {
    port = openstack_networking_port_v2.conductor.id
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
  # the OS is a deliberate act (taint + apply), not a side effect of a docs tweak.
  lifecycle {
    ignore_changes = [user_data]
  }
}

# ---------------------------------------------------------------------------
# Floating IP — public address, associated with the instance.
# ---------------------------------------------------------------------------

resource "openstack_networking_floatingip_v2" "conductor" {
  pool = data.openstack_networking_network_v2.external.name
}

resource "openstack_networking_floatingip_associate_v2" "conductor" {
  floating_ip = openstack_networking_floatingip_v2.conductor.address
  port_id     = openstack_networking_port_v2.conductor.id
}
