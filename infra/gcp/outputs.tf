output "deployment_enabled" {
  description = "True only after both the opt-in and acknowledgement are supplied."
  value       = local.enabled
}

output "planned_bucket" {
  description = "Bucket that will be created only when deployment is explicitly enabled."
  value       = local.enabled ? var.bucket_name : null
}

output "cloud_run_service" {
  description = "Private Cloud Run service name after a real apply."
  value       = local.enabled ? google_cloud_run_v2_service.runtime[0].name : null
}
