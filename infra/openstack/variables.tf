variable "os_cloud" {
  description = "Name of the cloud entry in ~/.config/openstack/clouds.yaml"
  type        = string
  default     = "openstack"
}

variable "instance_name" {
  description = "Name of the conductor instance"
  type        = string
  default     = "presemble-conductor"
}

variable "flavor_name" {
  description = "Flavor. All Bahnhof flavors have Disk=0, so the instance boots from a Cinder volume (see block_device below)."
  type        = string
  default     = "S - Small"
}

variable "image_name" {
  description = "Glance image to build the boot volume from"
  type        = string
  default     = "Debian13"
}

variable "network_name" {
  description = "Tenant network the instance attaches to (Bahnhof pre-provisions this)"
  type        = string
  default     = "default-network"
}

variable "external_network_name" {
  description = "External network that provides floating IPs"
  type        = string
  default     = "Public-Network"
}

variable "keypair_name" {
  description = "Name of the OpenStack keypair to inject (now Tofu-managed from ssh_public_key_path)."
  type        = string
  default     = "presemble"
}

variable "ssh_public_key_path" {
  description = "Path to the PUBLIC key uploaded as the keypair. Dedicated presemble local-dev key; the private half stays in ~/.ssh (never in Tofu state)."
  type        = string
  default     = "~/.ssh/presemble_ed25519.pub"
}

variable "caddy_editor_hash" {
  description = "Caddy bcrypt hash for the stopgap live-edit basic_auth (user 'editor'). Sourced from ~/.config/presemble/presemble-auth.env by the just recipes — never in the repo. Rendered into cloud-init via templatefile."
  type        = string
  sensitive   = true
}

variable "volume_size" {
  description = "Boot volume size in GB"
  type        = number
  default     = 20
}

variable "availability_zone" {
  description = "AZ to place the instance in"
  type        = string
  default     = "STE02"
}

variable "ssh_allowed_cidr" {
  description = "CIDR permitted to reach SSH (port 22). 0.0.0.0/0 = anywhere (auth is key-only)."
  type        = string
  default     = "0.0.0.0/0"
}
