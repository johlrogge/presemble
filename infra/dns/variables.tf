variable "domain" {
  description = "DNSimple-hosted zone for presemble"
  type        = string
  default     = "presemble.pub"
}

variable "conductor_subdomain" {
  description = "Record name for the conductor box, relative to the zone (\"\" = apex)"
  type        = string
  default     = "conductor"
}

variable "floating_ip" {
  description = "Public address of the conductor. Looked up from the openstack module: tofu -chdir=infra/openstack output -raw floating_ip (wired by `just dns-apply`)."
  type        = string

  validation {
    condition     = can(regex("^\\d{1,3}\\.\\d{1,3}\\.\\d{1,3}\\.\\d{1,3}$", var.floating_ip))
    error_message = "floating_ip must be an IPv4 dotted-quad."
  }
}

variable "record_ttl" {
  description = "TTL for the conductor A record. Low while bringing up; raise once stable."
  type        = number
  default     = 300
}

variable "dnsimple_sandbox" {
  description = "Target the DNSimple sandbox instead of production."
  type        = bool
  default     = false
}

# ---------------------------------------------------------------------------
# Alpha box DNS — additive only. The conductor record above is untouched.
# ---------------------------------------------------------------------------

variable "alpha_floating_ip" {
  description = "Public address of the alpha box. Looked up from the alpha module: tofu -chdir=infra/alpha output -raw floating_ip (wired by `just dns-apply-alpha`). Leave unset when only managing conductor records."
  type        = string
  default     = ""

  validation {
    condition     = var.alpha_floating_ip == "" || can(regex("^\\d{1,3}\\.\\d{1,3}\\.\\d{1,3}\\.\\d{1,3}$", var.alpha_floating_ip))
    error_message = "alpha_floating_ip must be an IPv4 dotted-quad or empty string."
  }
}
