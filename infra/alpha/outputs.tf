output "floating_ip" {
  description = "Public address of the alpha box"
  value       = openstack_networking_floatingip_v2.presemble_alpha.address
}

output "instance_id" {
  value = openstack_compute_instance_v2.presemble_alpha.id
}

output "ssh" {
  description = "Copy-paste SSH command"
  value       = "ssh debian@${openstack_networking_floatingip_v2.presemble_alpha.address}"
}
