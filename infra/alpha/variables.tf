variable "os_cloud" {
  description = "Name of the cloud entry in ~/.config/openstack/clouds.yaml"
  type        = string
  default     = "openstack"
}

variable "instance_name" {
  description = "Name of the alpha instance"
  type        = string
  default     = "presemble-alpha"
}

variable "flavor_name" {
  description = "Flavor. All Bahnhof flavors have Disk=0, so the instance boots from a Cinder volume (see block_device below). VERIFY against the OpenStack catalog with `openstack flavor list` before applying — the exact string must match the catalog name."
  type        = string
  default     = "M - Medium"
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
  description = "Name of the existing OpenStack keypair to inject. The presemble keypair was uploaded once by hand (or via infra/openstack); it is referenced here via a data lookup — NOT re-managed. Both modules share it."
  type        = string
  default     = "presemble"
}

variable "presemble_session_key" {
  description = "Secret session key for the control-plane service. Sourced from ~/.config/presemble/alpha-secrets.env by the just recipes — never in the repo. Rendered into cloud-init via templatefile."
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
