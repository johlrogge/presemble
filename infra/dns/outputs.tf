output "conductor_fqdn" {
  description = "Fully-qualified name the conductor record resolves"
  value       = trim("${var.conductor_subdomain}.${var.domain}", ".")
}

output "conductor_ip" {
  description = "Address the conductor record points at"
  value       = dnsimple_zone_record.conductor.value
}
