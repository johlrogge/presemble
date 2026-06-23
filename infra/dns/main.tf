# ---------------------------------------------------------------------------
# DNS for presemble — RECORDS ONLY.
#
# We deliberately do NOT manage the zone (dnsimple_zone) here. The zone is
# created once when the domain is registered/delegated to DNSimple; importing it
# into Tofu would put every record in this config's blast radius, so a state
# drift or a stray `destroy` could wipe records set in the web UI. Managing only
# the records we create keeps Tofu's reach to exactly what it provisioned.
# ---------------------------------------------------------------------------

resource "dnsimple_zone_record" "conductor" {
  zone_name = var.domain
  name      = var.conductor_subdomain
  type      = "A"
  value     = var.floating_ip
  ttl       = var.record_ttl

  # The conductor's public name is load-bearing once it's live; an accidental
  # `tofu destroy` should not be able to drop it. Remove this guard deliberately
  # if you ever need to tear the record down.
  lifecycle {
    prevent_destroy = true
  }
}
