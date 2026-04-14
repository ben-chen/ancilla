output "vpc_id" {
  description = "Created VPC ID."
  value       = aws_vpc.this.id
}

output "public_subnet_ids" {
  description = "Created public subnet IDs."
  value       = aws_subnet.public[*].id
}

output "private_app_subnet_ids" {
  description = "Created private application subnet IDs."
  value       = aws_subnet.private_app[*].id
}

output "private_db_subnet_ids" {
  description = "Created private database subnet IDs."
  value       = aws_subnet.private_db[*].id
}

output "nat_gateway_id" {
  description = "Created NAT gateway ID."
  value       = aws_nat_gateway.this.id
}
