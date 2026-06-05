# Auth comes from ~/.config/openstack/clouds.yaml via the named cloud entry.
# No credentials live in this repo. Select the cloud with var.os_cloud.
provider "openstack" {
  cloud = var.os_cloud
}
