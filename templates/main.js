$(document).ready(function () {
	var table = $('#myTable').DataTable({
	  paging: false,
	  ordering: true,
	  select: {
		items: 'row'
	  },
	  autoWidth: false,
	  responsive: true,
	  fixedColumns:   {
		heightMatch: 'none'
	  },
	  order: [[5, 'desc'], [1, 'desc'], [3, 'desc'], [2, 'desc'], [0, 'desc']],
	  searching: false,
	});

	$("#switch").addClass('checked');
	$("#switch").on('click', function() {
		
		if ($(this).hasClass('checked')) {
			document.documentElement.setAttribute('data-bs-theme', 'light');
			//$(this).text("DARK");
			console.log("Switched to light");
		}
		else {
			document.documentElement.setAttribute('data-bs-theme', 'dark');
			//$(this).text("LIGHT");
			console.log("Switched to dark");
		}
		$(this).toggleClass("checked");
	});

	console.log('JS done loading');
  });
