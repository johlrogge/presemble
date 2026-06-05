output "floating_ip" {
  description = "Public address of the conductor"
  value       = openstack_networking_floatingip_v2.conductor.address
}

output "instance_id" {
  value = openstack_compute_instance_v2.conductor.id
}

output "ssh" {
  description = "Copy-paste SSH command"
  value       = "ssh debian@${openstack_networking_floatingip_v2.conductor.address}"
}
