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
  description = "Existing keypair to inject. Managed outside Tofu (uploaded once via Horizon/CLI)."
  type        = string
  default     = "johlrogge"
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
