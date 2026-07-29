output "conductor_fqdn" {
  description = "Fully-qualified name the conductor record resolves"
  value       = trim("${var.conductor_subdomain}.${var.domain}", ".")
}

output "conductor_ip" {
  description = "Address the conductor record points at"
  value       = dnsimple_zone_record.conductor.value
}

output "alpha_app_fqdn" {
  description = "FQDN of the alpha control-plane (empty when alpha_floating_ip is not set)"
  value       = length(dnsimple_zone_record.alpha_app) > 0 ? "app.${var.domain}" : ""
}

output "alpha_wildcard_ip" {
  description = "Address the alpha wildcard record points at (empty when alpha_floating_ip is not set)"
  value       = length(dnsimple_zone_record.alpha_wildcard) > 0 ? var.alpha_floating_ip : ""
}
