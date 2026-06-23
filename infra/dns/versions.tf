terraform {
  required_version = ">= 1.6"

  required_providers {
    dnsimple = {
      source  = "dnsimple/dnsimple"
      version = "~> 1.0"
    }
  }
}
