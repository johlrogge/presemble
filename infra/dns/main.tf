# ---------------------------------------------------------------------------
# DNS for presemble — RECORDS ONLY.
#
# We deliberately do NOT manage the zone (dnsimple_zone) here. The zone is
# created once when the domain is registered/delegated to DNSimple; importing it
# into Tofu would put every record in this config's blast radius, so a state
# drift or a stray `destroy` could wipe records set in the web UI. Managing only
# the records we create keeps Tofu's reach to exactly what it provisioned.
# ---------------------------------------------------------------------------

# ---------------------------------------------------------------------------
# Alpha box DNS records — additive only.
# These are gated on alpha_floating_ip being set so that existing dns-plan /
# dns-apply runs (which don't pass -var alpha_floating_ip) stay unchanged.
# ---------------------------------------------------------------------------

resource "dnsimple_zone_record" "alpha_app" {
  count = var.alpha_floating_ip != "" ? 1 : 0

  zone_name = var.domain
  name      = "app"
  type      = "A"
  value     = var.alpha_floating_ip
  ttl       = var.record_ttl

  # prevent_destroy is intentionally left OUT for initial provisioning so the
  # record can be cleanly removed if the alpha IP changes before DNS stabilises.
  # ENABLE THIS after the first successful deploy + DNS wiring:
  #
  #   lifecycle {
  #     prevent_destroy = true
  #   }
}

resource "dnsimple_zone_record" "alpha_wildcard" {
  count = var.alpha_floating_ip != "" ? 1 : 0

  zone_name = var.domain
  name      = "*"
  type      = "A"
  value     = var.alpha_floating_ip
  ttl       = var.record_ttl

  # prevent_destroy is intentionally left OUT for initial provisioning.
  # ENABLE THIS after the first successful deploy + DNS wiring:
  #
  #   lifecycle {
  #     prevent_destroy = true
  #   }
}

# ---------------------------------------------------------------------------
# Conductor record (unchanged — do not edit).
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
