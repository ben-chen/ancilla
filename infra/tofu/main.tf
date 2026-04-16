data "aws_caller_identity" "current" {}
data "aws_partition" "current" {}
data "aws_ssm_parameter" "ecs_x86_ami" {
  name = "/aws/service/ecs/optimized-ami/amazon-linux-2/recommended/image_id"
}

data "aws_ssm_parameter" "ecs_gpu_ami" {
  name = "/aws/service/ecs/optimized-ami/amazon-linux-2/gpu/recommended/image_id"
}

data "aws_route53_zone" "app" {
  count        = var.domain_name != null && trimspace(var.domain_name) != "" ? 1 : 0
  name         = "${trimspace(coalesce(var.domain_name, ""))}."
  private_zone = false
}

module "network" {
  source = "./modules/network"
  count  = var.create_network ? 1 : 0

  name_prefix              = var.name_prefix
  aws_region               = var.aws_region
  app_env                  = var.app_env
  vpc_cidr                 = var.vpc_cidr
  availability_zones       = var.availability_zones
  public_subnet_cidrs      = var.public_subnet_cidrs
  private_app_subnet_cidrs = var.private_app_subnet_cidrs
  private_db_subnet_cidrs  = var.private_db_subnet_cidrs
}

locals {
  service_name           = "${var.name_prefix}-mvp"
  ecr_image              = "${aws_ecr_repository.app.repository_url}:${var.container_image_tag}"
  container_image        = coalesce(var.container_image, local.ecr_image)
  embedder_ecr_image     = "${aws_ecr_repository.embedder.repository_url}:${var.embedder_image_tag}"
  embedder_image         = coalesce(var.embedder_image, local.embedder_ecr_image)
  vpc_id                 = var.create_network ? module.network[0].vpc_id : var.vpc_id
  public_subnet_ids      = var.create_network ? module.network[0].public_subnet_ids : var.public_subnet_ids
  private_app_subnet_ids = var.create_network ? module.network[0].private_app_subnet_ids : var.private_app_subnet_ids
  private_db_subnet_ids  = var.create_network ? module.network[0].private_db_subnet_ids : var.private_db_subnet_ids
  db_engine_version      = coalesce(var.db_engine_version, var.aurora_engine_version, "15.8")
  db_instance_class      = coalesce(var.db_instance_class, var.aurora_instance_class, "db.t4g.micro")
  database_url           = "postgres://${var.db_username}:${random_password.db_password.result}@${aws_db_instance.postgres.address}:${aws_db_instance.postgres.port}/${var.db_name}?sslmode=require"
  bucket_name            = "${var.name_prefix}-${data.aws_caller_identity.current.account_id}-${var.aws_region}-memory-assets"
  embedder_accelerator   = lower(var.embedder_accelerator)
  embedder_is_gpu        = local.embedder_accelerator == "gpu"
  embedder_private_url   = var.embedder_enabled ? "http://${aws_instance.embedder[0].private_ip}:${var.embedder_port}" : null
  domain_enabled         = var.domain_name != null && trimspace(var.domain_name) != ""
  domain_name            = local.domain_enabled ? trimspace(var.domain_name) : null
  api_hostname           = local.domain_enabled && var.create_api_record ? "api.${local.domain_name}" : null
  app_hostnames = local.domain_enabled ? concat(
    [local.domain_name],
    var.create_www_record ? ["www.${local.domain_name}"] : [],
    var.create_api_record ? ["api.${local.domain_name}"] : [],
  ) : []
  tags = {
    Project     = "ancilla"
    Environment = var.app_env
    ManagedBy   = "terraform-or-tofu"
  }
}

resource "random_password" "db_password" {
  length  = 32
  special = false
}

resource "random_password" "basic_auth_password" {
  count   = var.basic_auth_enabled ? 1 : 0
  length  = 24
  special = false
}

resource "aws_ecr_repository" "app" {
  name                 = local.service_name
  image_tag_mutability = "MUTABLE"

  image_scanning_configuration {
    scan_on_push = true
  }
}

resource "aws_ecr_repository" "embedder" {
  name                 = "${local.service_name}-embedder"
  image_tag_mutability = "MUTABLE"

  image_scanning_configuration {
    scan_on_push = true
  }
}

resource "aws_cloudwatch_log_group" "app" {
  name              = "/ecs/${local.service_name}"
  retention_in_days = var.log_retention_days
}

resource "aws_cloudwatch_log_group" "embedder" {
  name              = "/ec2/${local.service_name}-embedder"
  retention_in_days = var.log_retention_days
}

resource "aws_s3_bucket" "assets" {
  bucket = local.bucket_name
}

resource "aws_s3_bucket_versioning" "assets" {
  bucket = aws_s3_bucket.assets.id

  versioning_configuration {
    status = "Enabled"
  }
}

resource "aws_s3_bucket_server_side_encryption_configuration" "assets" {
  bucket = aws_s3_bucket.assets.id

  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm = "AES256"
    }
  }
}

resource "aws_s3_bucket_public_access_block" "assets" {
  bucket                  = aws_s3_bucket.assets.id
  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_acm_certificate" "app" {
  count                     = local.domain_enabled ? 1 : 0
  domain_name               = local.domain_name
  subject_alternative_names = concat(
    var.create_www_record ? ["www.${local.domain_name}"] : [],
    var.create_api_record ? ["api.${local.domain_name}"] : [],
  )
  validation_method         = "DNS"

  lifecycle {
    create_before_destroy = true
  }

  tags = merge(local.tags, {
    Name = "${local.service_name}-tls"
  })
}

resource "aws_route53_record" "acm_validation" {
  for_each = local.domain_enabled ? {
    for option in aws_acm_certificate.app[0].domain_validation_options : option.domain_name => {
      name   = option.resource_record_name
      record = option.resource_record_value
      type   = option.resource_record_type
    }
  } : {}

  zone_id         = data.aws_route53_zone.app[0].zone_id
  name            = each.value.name
  type            = each.value.type
  ttl             = 60
  records         = [each.value.record]
  allow_overwrite = true
}

resource "aws_acm_certificate_validation" "app" {
  count = local.domain_enabled && var.enable_https_listener ? 1 : 0

  certificate_arn         = aws_acm_certificate.app[0].arn
  validation_record_fqdns = [for record in aws_route53_record.acm_validation : record.fqdn]
}

resource "aws_security_group" "alb" {
  count       = local.domain_enabled ? 1 : 0
  name        = "${local.service_name}-alb"
  description = "Ancilla MVP public application load balancer"
  vpc_id      = local.vpc_id

  ingress {
    from_port   = 80
    to_port     = 80
    protocol    = "tcp"
    cidr_blocks = ["0.0.0.0/0"]
  }

  ingress {
    from_port   = 443
    to_port     = 443
    protocol    = "tcp"
    cidr_blocks = ["0.0.0.0/0"]
  }

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }
}

resource "aws_lb" "app" {
  count              = local.domain_enabled ? 1 : 0
  name               = local.service_name
  load_balancer_type = "application"
  internal           = false
  security_groups    = [aws_security_group.alb[0].id]
  subnets            = local.public_subnet_ids

  tags = merge(local.tags, {
    Name = local.service_name
  })
}

resource "aws_lb_target_group" "app" {
  count       = local.domain_enabled ? 1 : 0
  name        = local.service_name
  port        = var.app_port
  protocol    = "HTTP"
  target_type = "ip"
  vpc_id      = local.vpc_id

  health_check {
    enabled             = true
    path                = "/healthz"
    protocol            = "HTTP"
    matcher             = "200"
    healthy_threshold   = 2
    unhealthy_threshold = 3
    interval            = 15
    timeout             = 5
  }

  tags = merge(local.tags, {
    Name = local.service_name
  })
}

resource "aws_lb_listener" "http_forward" {
  count             = local.domain_enabled && !var.enable_https_listener ? 1 : 0
  load_balancer_arn = aws_lb.app[0].arn
  port              = 80
  protocol          = "HTTP"

  default_action {
    type             = "forward"
    target_group_arn = aws_lb_target_group.app[0].arn
  }
}

resource "aws_lb_listener" "http_redirect" {
  count             = local.domain_enabled && var.enable_https_listener ? 1 : 0
  load_balancer_arn = aws_lb.app[0].arn
  port              = 80
  protocol          = "HTTP"

  default_action {
    type = "redirect"

    redirect {
      port        = "443"
      protocol    = "HTTPS"
      status_code = "HTTP_301"
    }
  }
}

resource "aws_lb_listener" "https" {
  count             = local.domain_enabled && var.enable_https_listener ? 1 : 0
  load_balancer_arn = aws_lb.app[0].arn
  port              = 443
  protocol          = "HTTPS"
  ssl_policy        = "ELBSecurityPolicy-TLS13-1-2-2021-06"
  certificate_arn   = aws_acm_certificate_validation.app[0].certificate_arn

  default_action {
    type             = "forward"
    target_group_arn = aws_lb_target_group.app[0].arn
  }
}

resource "aws_route53_record" "app_alias" {
  for_each = local.domain_enabled ? toset(local.app_hostnames) : toset([])

  zone_id = data.aws_route53_zone.app[0].zone_id
  name    = each.value
  type    = "A"

  alias {
    name                   = aws_lb.app[0].dns_name
    zone_id                = aws_lb.app[0].zone_id
    evaluate_target_health = true
  }
}

resource "aws_security_group" "ecs_service" {
  name        = "${local.service_name}-ecs"
  description = "Ancilla MVP public ECS tasks"
  vpc_id      = local.vpc_id

  ingress {
    from_port   = var.app_port
    to_port     = var.app_port
    protocol    = "tcp"
    cidr_blocks = var.allowed_ingress_cidr_blocks
  }

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }
}

resource "aws_security_group" "embedder" {
  count       = var.embedder_enabled ? 1 : 0
  name        = "${local.service_name}-embedder"
  description = "Ancilla embedder service"
  vpc_id      = local.vpc_id

  ingress {
    from_port       = var.embedder_port
    to_port         = var.embedder_port
    protocol        = "tcp"
    security_groups = [aws_security_group.ecs_service.id]
  }

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }
}

resource "aws_security_group" "db" {
  name        = "${local.service_name}-db"
  description = "Ancilla MVP PostgreSQL"
  vpc_id      = local.vpc_id

  ingress {
    from_port       = 5432
    to_port         = 5432
    protocol        = "tcp"
    security_groups = [aws_security_group.ecs_service.id]
  }

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }
}

resource "aws_db_subnet_group" "postgres" {
  name       = "${local.service_name}-db-subnets"
  subnet_ids = local.private_db_subnet_ids
}

resource "aws_db_instance" "postgres" {
  identifier                      = "${local.service_name}-postgres"
  engine                          = "postgres"
  engine_version                  = local.db_engine_version
  instance_class                  = local.db_instance_class
  db_name                         = var.db_name
  username                        = var.db_username
  password                        = random_password.db_password.result
  allocated_storage               = var.db_allocated_storage_gb
  max_allocated_storage           = var.db_max_allocated_storage_gb
  storage_type                    = var.db_storage_type
  db_subnet_group_name            = aws_db_subnet_group.postgres.name
  vpc_security_group_ids          = [aws_security_group.db.id]
  backup_retention_period         = var.backup_retention_period
  storage_encrypted               = true
  deletion_protection             = false
  skip_final_snapshot             = true
  copy_tags_to_snapshot           = true
  enabled_cloudwatch_logs_exports = ["postgresql"]
  publicly_accessible             = false
}

resource "aws_secretsmanager_secret" "database_url" {
  name = "${local.service_name}/database-url"
}

resource "aws_secretsmanager_secret" "basic_auth_password" {
  count = var.basic_auth_enabled ? 1 : 0
  name  = "${local.service_name}/basic-auth-password"
}

resource "aws_secretsmanager_secret_version" "database_url" {
  secret_id     = aws_secretsmanager_secret.database_url.id
  secret_string = local.database_url
}

resource "aws_secretsmanager_secret_version" "basic_auth_password" {
  count         = var.basic_auth_enabled ? 1 : 0
  secret_id     = aws_secretsmanager_secret.basic_auth_password[0].id
  secret_string = random_password.basic_auth_password[0].result
}

data "aws_iam_policy_document" "ecs_task_assume_role" {
  statement {
    actions = ["sts:AssumeRole"]

    principals {
      type        = "Service"
      identifiers = ["ecs-tasks.amazonaws.com"]
    }
  }
}

resource "aws_iam_role" "ecs_execution" {
  name               = "${local.service_name}-execution"
  assume_role_policy = data.aws_iam_policy_document.ecs_task_assume_role.json
}

resource "aws_iam_role_policy_attachment" "ecs_execution_managed" {
  role       = aws_iam_role.ecs_execution.name
  policy_arn = "arn:${data.aws_partition.current.partition}:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy"
}

data "aws_iam_policy_document" "ecs_execution_extra" {
  statement {
    sid     = "ReadDatabaseSecret"
    actions = ["secretsmanager:GetSecretValue"]
    resources = concat(
      [aws_secretsmanager_secret.database_url.arn],
      var.basic_auth_enabled ? [aws_secretsmanager_secret.basic_auth_password[0].arn] : [],
    )
  }
}

resource "aws_iam_role_policy" "ecs_execution_extra" {
  name   = "${local.service_name}-execution-extra"
  role   = aws_iam_role.ecs_execution.id
  policy = data.aws_iam_policy_document.ecs_execution_extra.json
}

data "aws_iam_policy_document" "embedder_instance_assume_role" {
  statement {
    actions = ["sts:AssumeRole"]

    principals {
      type        = "Service"
      identifiers = ["ec2.amazonaws.com"]
    }
  }
}

resource "aws_iam_role" "embedder_instance" {
  count              = var.embedder_enabled ? 1 : 0
  name               = "${local.service_name}-embedder-instance"
  assume_role_policy = data.aws_iam_policy_document.embedder_instance_assume_role.json
}

resource "aws_iam_role_policy_attachment" "embedder_instance_ssm" {
  count      = var.embedder_enabled ? 1 : 0
  role       = aws_iam_role.embedder_instance[0].name
  policy_arn = "arn:${data.aws_partition.current.partition}:iam::aws:policy/AmazonSSMManagedInstanceCore"
}

resource "aws_iam_role_policy_attachment" "embedder_instance_ecr" {
  count      = var.embedder_enabled ? 1 : 0
  role       = aws_iam_role.embedder_instance[0].name
  policy_arn = "arn:${data.aws_partition.current.partition}:iam::aws:policy/AmazonEC2ContainerRegistryReadOnly"
}

data "aws_iam_policy_document" "embedder_instance" {
  statement {
    sid = "WriteEmbedderLogs"
    actions = [
      "logs:CreateLogStream",
      "logs:DescribeLogStreams",
      "logs:PutLogEvents",
    ]
    resources = ["${aws_cloudwatch_log_group.embedder.arn}:*"]
  }
}

resource "aws_iam_role_policy" "embedder_instance" {
  count  = var.embedder_enabled ? 1 : 0
  name   = "${local.service_name}-embedder-instance"
  role   = aws_iam_role.embedder_instance[0].id
  policy = data.aws_iam_policy_document.embedder_instance.json
}

resource "aws_iam_instance_profile" "embedder_instance" {
  count = var.embedder_enabled ? 1 : 0
  name  = "${local.service_name}-embedder-instance"
  role  = aws_iam_role.embedder_instance[0].name
}

resource "aws_iam_role" "ecs_task" {
  name               = "${local.service_name}-task"
  assume_role_policy = data.aws_iam_policy_document.ecs_task_assume_role.json
}

data "aws_iam_policy_document" "ecs_task" {
  statement {
    sid = "InvokeBedrock"
    actions = [
      "bedrock:InvokeModel",
      "bedrock:InvokeModelWithResponseStream",
    ]
    resources = ["*"]
  }

  statement {
    sid = "SynthesizeSpeechWithPolly"
    actions = [
      "polly:SynthesizeSpeech",
    ]
    resources = ["*"]
  }

  statement {
    sid = "AccessAncillaAssetsBucket"
    actions = [
      "s3:GetObject",
      "s3:PutObject",
      "s3:DeleteObject",
    ]
    resources = ["${aws_s3_bucket.assets.arn}/*"]
  }

  statement {
    sid       = "ListAncillaAssetsBucket"
    actions   = ["s3:ListBucket"]
    resources = [aws_s3_bucket.assets.arn]
  }
}

resource "aws_iam_role_policy" "ecs_task" {
  name   = "${local.service_name}-task"
  role   = aws_iam_role.ecs_task.id
  policy = data.aws_iam_policy_document.ecs_task.json
}

resource "aws_ecs_cluster" "app" {
  name = local.service_name
}

resource "aws_instance" "embedder" {
  count                       = var.embedder_enabled ? 1 : 0
  ami                         = local.embedder_is_gpu ? data.aws_ssm_parameter.ecs_gpu_ami.value : data.aws_ssm_parameter.ecs_x86_ami.value
  instance_type               = var.embedder_instance_type
  subnet_id                   = local.public_subnet_ids[0]
  vpc_security_group_ids      = [aws_security_group.embedder[0].id]
  iam_instance_profile        = aws_iam_instance_profile.embedder_instance[0].name
  associate_public_ip_address = true
  user_data_replace_on_change = true
  user_data = templatefile("${path.module}/templates/embedder-user-data.sh.tftpl", {
    aws_region       = var.aws_region
    embedder_image   = local.embedder_image
    embedder_port    = var.embedder_port
    embedder_device  = var.embedder_device
    embedder_use_gpu = local.embedder_is_gpu
    batch_size       = var.embedder_batch_size
    max_length       = var.embedder_max_length
    default_model_id = var.embedding_memory_model_id
    log_group_name   = aws_cloudwatch_log_group.embedder.name
    registry_host    = split("/", aws_ecr_repository.embedder.repository_url)[0]
  })

  root_block_device {
    encrypted   = true
    volume_size = var.embedder_root_volume_size_gb
    volume_type = "gp3"
  }

  metadata_options {
    http_endpoint = "enabled"
    http_tokens   = "required"
  }

  tags = merge(local.tags, {
    Name = "${local.service_name}-embedder"
  })
}

resource "aws_ecs_task_definition" "app" {
  family                   = local.service_name
  requires_compatibilities = ["FARGATE"]
  network_mode             = "awsvpc"
  cpu                      = tostring(var.task_cpu)
  memory                   = tostring(var.task_memory)
  execution_role_arn       = aws_iam_role.ecs_execution.arn
  task_role_arn            = aws_iam_role.ecs_task.arn

  runtime_platform {
    cpu_architecture        = "ARM64"
    operating_system_family = "LINUX"
  }

  container_definitions = jsonencode([
    {
      name      = "ancilla"
      image     = local.container_image
      essential = true
      portMappings = [
        {
          containerPort = var.app_port
          hostPort      = var.app_port
          protocol      = "tcp"
        }
      ]
      environment = concat(
        [
          { name = "ANCILLA_APP_ENV", value = var.app_env },
          { name = "AWS_REGION", value = var.aws_region },
          { name = "AWS_DEFAULT_REGION", value = var.aws_region },
          { name = "BEDROCK_CHAT_MODEL_ID", value = var.bedrock_chat_model_id },
          { name = "BEDROCK_CHAT_MODELS_JSON", value = jsonencode(var.bedrock_chat_models) },
          { name = "BEDROCK_CHAT_MAX_TOKENS", value = tostring(var.bedrock_chat_max_tokens) },
          { name = "BEDROCK_CHAT_TEMPERATURE", value = tostring(var.bedrock_chat_temperature) },
          { name = "ANCILLA_SERVER_LOCAL_EMBED_MODEL", value = var.embedding_memory_model_id },
          { name = "ANCILLA_SERVER_EMBEDDER_TIMEOUT_SECONDS", value = tostring(var.embedder_timeout_seconds) }
        ],
        concat(
          var.basic_auth_enabled ? [
            { name = "ANCILLA_SERVER_BASIC_AUTH_USERNAME", value = var.basic_auth_username }
          ] : [],
          var.bedrock_gate_model_id != null ? [
            { name = "BEDROCK_GATE_MODEL_ID", value = var.bedrock_gate_model_id }
          ] : [],
          var.embedder_enabled ? [
            { name = "ANCILLA_SERVER_EMBEDDER_BASE_URL", value = local.embedder_private_url }
          ] : []
        )
      )
      secrets = concat(
        [
          {
            name      = "DATABASE_URL"
            valueFrom = aws_secretsmanager_secret.database_url.arn
          }
        ],
        var.basic_auth_enabled ? [
          {
            name      = "ANCILLA_SERVER_BASIC_AUTH_PASSWORD"
            valueFrom = aws_secretsmanager_secret.basic_auth_password[0].arn
          }
        ] : []
      )
      logConfiguration = {
        logDriver = "awslogs"
        options = {
          awslogs-group         = aws_cloudwatch_log_group.app.name
          awslogs-region        = var.aws_region
          awslogs-stream-prefix = "ancilla"
        }
      }
    }
  ])
}

resource "aws_ecs_service" "app" {
  name                               = local.service_name
  cluster                            = aws_ecs_cluster.app.id
  task_definition                    = aws_ecs_task_definition.app.arn
  desired_count                      = var.desired_count
  launch_type                        = "FARGATE"
  enable_execute_command             = true
  deployment_maximum_percent         = 200
  deployment_minimum_healthy_percent = 50

  network_configuration {
    subnets          = local.public_subnet_ids
    security_groups  = [aws_security_group.ecs_service.id]
    assign_public_ip = true
  }

  dynamic "load_balancer" {
    for_each = local.domain_enabled ? [1] : []

    content {
      target_group_arn = aws_lb_target_group.app[0].arn
      container_name   = "ancilla"
      container_port   = var.app_port
    }
  }

  depends_on = [
    aws_lb_listener.http_forward,
    aws_lb_listener.http_redirect,
    aws_lb_listener.https,
  ]
}
